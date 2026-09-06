//! An actively-exploited advisory needs a documented fast lane (Issue #95).
//!
//! The routine dependency path is deliberately slow: `cargo-audit.yml` and
//! `cargo-upgrade.yml` both wake at 06:00 UTC on a Monday, and every bump is
//! judged by `ci.yml`, `dependency-review.yml` and `cargo deny check` before it
//! merges. `scripts/crates-quarantine.sh` slows it further, refusing any
//! crates.io version younger than 24 hours. That is the right default for a
//! routine week and the wrong one for a CVE being exploited today.
//!
//! Without a written procedure the maintainer facing that case improvises a
//! merge path under pressure — and the improvisation that suggests itself is
//! an administrator merge past the very gates the repository relies on. These
//! tests read the committed policy and assert the fast lane is documented: how
//! the bump is raised out of cadence, what the quarantine does to a same-day
//! release, who approves it, and which gates it may never skip.

use std::path::{Path, PathBuf};

/// Repository root — `CARGO_MANIFEST_DIR` is `<root>/rebase`.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate directory has a parent")
        .to_path_buf()
}

/// A committed file, read from the working tree.
///
/// Panics when the file is absent: a missing policy is the failure this suite
/// exists to catch, never a skip.
fn committed(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be committed: {error}", path.display()))
}

/// One `## ` section of a Markdown document.
#[derive(Debug)]
struct Section {
    /// The heading text, without its `## ` marker.
    heading: String,
    /// Every line below the heading, up to the next `## `, verbatim.
    body: String,
}

/// Every `## ` section of a Markdown document, in file order.
///
/// Deeper headings (`### ` and below) stay inside the section they belong to,
/// so a procedure split into sub-steps is still read as one block. Content
/// above the first `## ` heading belongs to no section and is dropped.
fn sections(markdown: &str) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();

    for line in markdown.lines() {
        match line.strip_prefix("## ") {
            Some(heading) => sections.push(Section {
                heading: heading.trim().to_string(),
                body: String::new(),
            }),
            None => {
                if let Some(section) = sections.last_mut() {
                    section.body.push_str(line);
                    section.body.push('\n');
                }
            }
        }
    }

    sections
}

/// The emergency-override section of a policy document, or `None` when the
/// document carries no such section.
///
/// Matched on the heading rather than on a fixed title so the wording can be
/// improved without the gate going red for a rename.
fn override_section(markdown: &str) -> Option<Section> {
    sections(markdown).into_iter().find(|section| {
        let heading = section.heading.to_ascii_lowercase();
        heading.contains("emergency") || heading.contains("override")
    })
}

/// The emergency-override section of `SECURITY.md`, or a panic naming what the
/// document does carry instead.
fn documented_override() -> Section {
    let policy = committed("SECURITY.md");
    override_section(&policy).unwrap_or_else(|| {
        let headings: Vec<String> = sections(&policy)
            .into_iter()
            .map(|section| section.heading)
            .collect();
        panic!(
            "SECURITY.md must document an emergency-override path for an \
             actively-exploited advisory — a maintainer improvising one under \
             pressure reaches for an administrator merge. Found headings \
             {headings:?}"
        )
    })
}

#[test]
fn the_policy_documents_how_to_raise_a_bump_outside_the_weekly_cadence() {
    let section = documented_override();

    assert!(
        section.body.contains("cargo-upgrade.yml"),
        "the override must name the workflow that raises the bump, so the fast \
         lane runs the same gated path as the weekly one"
    );
    assert!(
        section.body.contains("workflow_dispatch"),
        "the override must name the trigger that starts it out of cadence — \
         `cargo-upgrade.yml` declares `workflow_dispatch` for exactly this"
    );
}

#[test]
fn the_policy_says_what_the_publish_age_quarantine_does_to_a_same_day_fix() {
    let section = documented_override();

    assert!(
        section.body.contains("crates-quarantine.sh"),
        "the fix for an exploited CVE is often published hours ago, so the \
         override must say what the publish-age gate does to it"
    );
    assert!(
        section.body.contains("24"),
        "the override must state the window it is working against, not just \
         that one exists"
    );
}

#[test]
fn the_policy_names_who_approves_an_out_of_cadence_bump() {
    let section = documented_override();

    assert!(
        section.body.contains("@stSoftwareAU/developers"),
        "an override with no named approver is an override anyone may take — \
         the section must name the reviewing team"
    );
}

#[test]
fn the_policy_states_which_gates_the_fast_lane_may_never_skip() {
    let section = documented_override();

    for gate in ["ci.yml", "dependency-review.yml", "cargo-audit.yml"] {
        assert!(
            section.body.contains(gate),
            "the override must name {gate} among the gates that still have to \
             pass — speeding up review is the point, skipping the gates is not"
        );
    }
    assert!(
        section.body.contains("--admin"),
        "the improvisation this section exists to replace is an administrator \
         merge past the required checks; the policy has to refuse it by name"
    );
}

#[test]
fn the_contributing_guide_points_at_the_documented_override() {
    let guide = committed("CONTRIBUTING.md");

    assert!(
        guide.contains("SECURITY.md"),
        "CONTRIBUTING.md documents the weekly cadence, so it must point at the \
         policy that documents leaving it — a maintainer reading the cadence \
         is the one who needs the exception"
    );
    let mentions_override = guide.to_ascii_lowercase().contains("emergency")
        || guide.to_ascii_lowercase().contains("override");
    assert!(
        mentions_override,
        "the pointer has to say what it points at: the cadence section must \
         name the emergency path, not just cite `SECURITY.md` for triage"
    );
}

#[test]
fn a_policy_with_no_override_section_is_rejected() {
    // The helpers must not report a pass for the shape this suite exists to
    // catch: a policy that covers inbound reporting and nothing else.
    let reporting_only = "# Security Policy\n\
                          \n\
                          ## Reporting a vulnerability\n\
                          \n\
                          Report privately to the maintainers.\n\
                          \n\
                          ## Scope\n\
                          \n\
                          An experimental research tool.\n";

    let parsed = sections(reporting_only);
    assert_eq!(
        parsed.len(),
        2,
        "two `## ` sections are declared, found {:?}",
        parsed
            .iter()
            .map(|section| &section.heading)
            .collect::<Vec<&String>>()
    );
    assert!(
        parsed[0].body.contains("Report privately"),
        "a section keeps the prose below its heading"
    );
    assert!(
        !parsed[0].body.contains("An experimental research tool"),
        "a section stops at the next `## ` heading"
    );
    assert!(
        override_section(reporting_only).is_none(),
        "a policy covering only inbound reporting documents no override path"
    );
}

#[test]
fn a_sub_heading_stays_inside_the_section_it_belongs_to() {
    // A procedure written as numbered sub-steps under `### ` headings is one
    // override section, not several — the gate reads the whole block.
    let with_steps = "## Emergency override\n\
                      \n\
                      ### Step one\n\
                      \n\
                      Dispatch `cargo-upgrade.yml`.\n\
                      \n\
                      ### Step two\n\
                      \n\
                      Ask `@stSoftwareAU/developers` to review.\n";

    let section = override_section(with_steps).expect("the heading names an override");
    assert_eq!(section.heading, "Emergency override");
    assert!(
        section.body.contains("cargo-upgrade.yml")
            && section.body.contains("@stSoftwareAU/developers"),
        "both sub-steps belong to the one section, found {:?}",
        section.body
    );
}
