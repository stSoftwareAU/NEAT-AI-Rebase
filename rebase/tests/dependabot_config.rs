//! The repository commits a Dependabot configuration (Issue #93).
//!
//! `.github/workflows/cargo-audit.yml` audits the committed `Cargo.lock`
//! against RustSec on every PR and weekly, but that is only the scanning half
//! of the supply-chain check. Dependabot's advisory feed is the second,
//! independent channel — the one a maintainer sees on the repository's
//! Security tab rather than only in a CI job log. Whether alerts are enabled
//! is a repository setting no static read can confirm, so a committed
//! `.github/dependabot.yml` is the only anchor available: it registers the
//! cargo ecosystem with Dependabot and says, in the tree, that the channel is
//! wired up.
//!
//! Version-update pull requests deliberately stay with
//! `.github/workflows/cargo-upgrade.yml`, which runs
//! `scripts/crates-quarantine.sh` over every version it newly resolves
//! (Issue #91). A second weekly bumper with no publish-age window would let a
//! freshly published version in through the door that gate exists to hold, so
//! the cargo entry pins `open-pull-requests-limit: 0`. These tests read the
//! committed configuration and assert both halves.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Repository root — `CARGO_MANIFEST_DIR` is `<root>/rebase`.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate directory has a parent")
        .to_path_buf()
}

/// The committed Dependabot configuration, read from the working tree.
///
/// Panics when the file is absent: a missing configuration is the failure this
/// suite exists to catch, never a skip.
fn dependabot_config() -> String {
    let path = repo_root().join(".github/dependabot.yml");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be committed: {error}", path.display()))
}

/// Strip one layer of matching YAML quotes from a scalar.
fn unquote(value: &str) -> &str {
    let value = value.trim();
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|v| v.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

/// Every entry under the top-level `updates:` key, flattened to dotted paths.
///
/// `schedule:` followed by an indented `interval: "weekly"` is reported as
/// `schedule.interval`, so a nested key cannot be satisfied by a same-named key
/// somewhere else in the entry. Only the block mapping form Dependabot's own
/// documentation uses is understood; comments and blank lines are ignored.
fn update_entries(config: &str) -> Vec<BTreeMap<String, String>> {
    let mut entries: Vec<BTreeMap<String, String>> = Vec::new();
    let mut parents: Vec<(usize, String)> = Vec::new();
    let mut in_updates = false;

    for line in config.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let indent = line.len() - line.trim_start().len();
        if indent == 0 {
            // A new top-level key ends whatever block was being read.
            in_updates = trimmed == "updates:";
            parents.clear();
            continue;
        }
        if !in_updates {
            continue;
        }

        // `- package-ecosystem: "cargo"` opens an entry and carries its first
        // key; the entry's remaining keys sit two columns further in.
        let (indent, trimmed) = match trimmed.strip_prefix("- ") {
            Some(rest) => {
                entries.push(BTreeMap::new());
                parents.clear();
                (indent + 2, rest.trim())
            }
            None => (indent, trimmed),
        };

        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        while parents.last().is_some_and(|(depth, _)| *depth >= indent) {
            parents.pop();
        }

        let key = key.trim();
        if value.trim().is_empty() {
            parents.push((indent, key.to_string()));
            continue;
        }

        let mut path: Vec<&str> = parents.iter().map(|(_, key)| key.as_str()).collect();
        path.push(key);
        let entry = entries
            .last_mut()
            .expect("a key under `updates:` belongs to a `- ` entry");
        entry.insert(path.join("."), unquote(value).to_string());
    }

    entries
}

/// The single `updates:` entry for a given ecosystem, or `None` when absent.
fn ecosystem_entry(config: &str, ecosystem: &str) -> Option<BTreeMap<String, String>> {
    update_entries(config)
        .into_iter()
        .find(|entry| entry.get("package-ecosystem").map(String::as_str) == Some(ecosystem))
}

#[test]
fn the_repository_commits_a_dependabot_configuration() {
    let config = dependabot_config();
    let version = config
        .lines()
        .find_map(|line| line.trim_end().strip_prefix("version:"))
        .expect("the configuration declares a top-level `version:`");
    assert_eq!(
        unquote(version),
        "2",
        "Dependabot only accepts schema version 2"
    );
}

#[test]
fn dependabot_watches_the_cargo_ecosystem_weekly() {
    let config = dependabot_config();
    let cargo = ecosystem_entry(&config, "cargo").expect(
        "`.github/dependabot.yml` must register the cargo ecosystem — \
         that registration is the committed anchor for the advisory channel",
    );

    assert_eq!(
        cargo.get("directory").map(String::as_str),
        Some("/"),
        "the workspace manifest sits at the repository root"
    );
    assert_eq!(
        cargo.get("schedule.interval").map(String::as_str),
        Some("weekly"),
        "the issue asks for a weekly cargo schedule, matching cargo-audit.yml"
    );
}

#[test]
fn cargo_version_update_pull_requests_stay_with_the_quarantined_workflow() {
    let config = dependabot_config();
    let cargo = ecosystem_entry(&config, "cargo").expect("the cargo ecosystem is registered");

    assert_eq!(
        cargo.get("open-pull-requests-limit").map(String::as_str),
        Some("0"),
        "`cargo-upgrade.yml` runs scripts/crates-quarantine.sh over every version it \
         newly resolves (Issue #91); a second bumper with no publish-age window would \
         walk straight past that gate"
    );
}

#[test]
fn cargo_updates_wait_out_a_publish_age_cooldown() {
    let config = dependabot_config();
    let cargo = ecosystem_entry(&config, "cargo").expect("the cargo ecosystem is registered");

    let days: u32 = cargo
        .get("cooldown.default-days")
        .expect("the cargo entry sets a `cooldown` publish-age window")
        .parse()
        .expect("`cooldown.default-days` is a whole number of days");
    assert!(
        days >= 7,
        "a newly published version can be malicious or unstable; Dependabot must hold \
         it for at least 7 days, found {days}"
    );
}

#[test]
fn a_configuration_without_a_cargo_entry_is_rejected() {
    // The parser must not report a pass for a configuration that registers some
    // other ecosystem — the shape this suite exists to catch.
    let config = "version: 2\n\
                  \n\
                  updates:\n\
                  \x20 - package-ecosystem: \"github-actions\"\n\
                  \x20   directory: \"/\"\n\
                  \x20   schedule:\n\
                  \x20     interval: \"weekly\"\n";

    assert!(ecosystem_entry(config, "cargo").is_none());
    let actions = ecosystem_entry(config, "github-actions").expect("the actions entry parses");
    assert_eq!(
        actions.get("schedule.interval").map(String::as_str),
        Some("weekly")
    );
    assert_eq!(actions.get("interval"), None, "nested keys stay dotted");
}
