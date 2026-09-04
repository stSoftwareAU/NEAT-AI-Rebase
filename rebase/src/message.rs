//! What a rebase run says it did (Issue #80).
//!
//! Three scores are in play, and each one comes from a different baseline:
//!
//! | Stage | Where it comes from | Named as |
//! | --- | --- | --- |
//! | **claimed** | the producer's own measurement, on the older creature it opened from | `claimed` |
//! | **validated** | this run's authoritative scorer, on the source creature — when it scored it at all | `validated source` |
//! | **champion** | this run's authoritative scorer, on the champion the replay was measured against | `champion` |
//! | **rebased** | this run's authoritative scorer, on the promoted candidate | `rebased` |
//!
//! [`SourceScore`] is what keeps the first two apart: a producer's claim and
//! an authoritative measurement of the same creature are different facts, and
//! only one of them can be compared with the champion on equal terms.
//!
//! Reporting two of those deltas without naming their baselines reads as a
//! contradiction — "declined by 0.0015" beside "improved by 0.000344" in one
//! sentence, where the first is against the producer's claim and the second
//! against the champion. Nothing here calls a claim mismatch a decline: the
//! creature did not get worse, two different measurements were compared. The
//! rebase gain is always written `champion X → rebased Y`, so the arrow
//! carries its own baseline, and the claim comparison is always written
//! `claim delta … vs claimed Z`.
//!
//! These messages become the `rebase` creature tag and the journal's `result`
//! detail, and downstream they become commit subjects, so they stay to one
//! line.

/// The score of the creature the discoveries came from, carrying who measured
/// it.
///
/// The two are not interchangeable and must never be printed in the same
/// words: a producer's claim was taken on a different, older creature, while a
/// validated figure comes from this run's own authoritative pass. Collapsing
/// them is what let a claim mismatch be reported as the creature declining.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SourceScore {
    /// The producer's own figure, measured on its opening creature and never
    /// re-measured here.
    Claimed(f64),
    /// This run's authoritative scorer measured the source creature itself.
    Validated(f64),
}

impl SourceScore {
    /// The number, whichever way it was measured.
    pub fn value(self) -> f64 {
        match self {
            Self::Claimed(v) | Self::Validated(v) => v,
        }
    }

    /// `delta vs baseline`, in the wording that names how the baseline was
    /// arrived at.
    fn against(self, score: f64) -> String {
        match self {
            Self::Claimed(v) => format!("claim delta {:+.2e} vs claimed {v:.6}", score - v),
            Self::Validated(v) => {
                format!("source delta {:+.2e} vs validated source {v:.6}", score - v)
            }
        }
    }
}

/// What a rebase reports about a promoted candidate.
#[derive(Debug, Clone, Copy)]
pub struct RebaseStamp<'a> {
    /// Authoritative score of the promoted candidate — the **rebased** score.
    pub score: f64,
    /// Authoritative error of the promoted candidate.
    pub error: f64,
    /// The champion's authoritative score from the same scorer call — the
    /// baseline the rebase gain is measured against.
    pub champion_score: f64,
    /// The score of the creature the enhancements came from, and whether it
    /// was claimed by its producer or validated here.
    pub source_score: SourceScore,
    /// How many enhancements the promoted candidate applied.
    pub applied: usize,
    /// Which cohort member won (`bundle`, `single-02`, …).
    pub label: &'a str,
    /// Where the enhancements came from (`neat-ai-forests`, `harvest`, …).
    pub source: &'a str,
}

/// What a rebase reports when the champion held.
#[derive(Debug, Clone, Copy)]
pub struct NoImprovement<'a> {
    /// The champion's authoritative score — the one that stood.
    pub champion_score: f64,
    /// The best candidate's authoritative score, when any candidate was
    /// scored. Absent is not zero: a verdict that scored nothing says so.
    pub best_score: Option<f64>,
    /// The source creature's score, as in [`RebaseStamp::source_score`].
    pub source_score: SourceScore,
    /// How many enhancements were carried into the authoritative pass.
    pub attempted: usize,
    /// Where the enhancements came from.
    pub source: &'a str,
}

/// Population skim line for a promoted candidate; becomes the sampler commit
/// subject.
///
/// The 🪢 prefix is the `rebase` tag's emoji (NEAT-AI-Rebase #12): a knot is
/// two lineages tied back together, which is exactly what a rebase does — an
/// improvement found on an older incumbent reconciled with the latest
/// champion, rather than one lineage replacing the other.
///
/// ```
/// use neat_ai_rebase::message::{RebaseStamp, SourceScore, rebase_message};
///
/// let line = rebase_message(&RebaseStamp {
///     score: 0.419751,
///     error: 0.580249,
///     champion_score: 0.419407,
///     source_score: SourceScore::Claimed(0.421251),
///     applied: 2,
///     label: "bundle",
///     source: "neat-ai-forests",
/// });
/// assert!(line.contains("champion 0.419407 → rebased 0.419751 (+3.44e-4)"));
/// assert!(line.contains("claim delta -1.50e-3 vs claimed 0.421251"));
/// ```
pub fn rebase_message(stamp: &RebaseStamp<'_>) -> String {
    format!(
        // `{:+.2e}` carries its own sign. The rebase gain is always positive —
        // nothing else is promoted — but the claim delta is routinely negative
        // and says something worth reading when it is: the producer measured
        // itself on an older, easier opening creature. A hardcoded `+` printed
        // that as `+-2.29e-6`.
        "🪢 Rebase applied · {} {} from {} · champion {:.6} → rebased {:.6} ({:+.2e}) · {}",
        stamp.applied,
        enhancement_noun(stamp.applied),
        stamp.source,
        stamp.champion_score,
        stamp.score,
        stamp.score - stamp.champion_score,
        stamp.source_score.against(stamp.score),
    )
}

/// Skim line for a run whose candidates were all rejected.
///
/// A no-improvement verdict is a normal result, and it is reported in the same
/// vocabulary as a win so the two can be read side by side in a log.
///
/// ```
/// use neat_ai_rebase::message::{NoImprovement, SourceScore, no_improvement_message};
///
/// let line = no_improvement_message(&NoImprovement {
///     champion_score: 0.5,
///     best_score: Some(0.49),
///     source_score: SourceScore::Claimed(0.6),
///     attempted: 2,
///     source: "neat-ai-forests",
/// });
/// assert!(line.contains("champion 0.500000 held"));
/// assert!(line.contains("best candidate 0.490000 (-1.00e-2)"));
/// ```
pub fn no_improvement_message(miss: &NoImprovement<'_>) -> String {
    let best = match miss.best_score {
        Some(best) => format!(
            "best candidate {:.6} ({:+.2e}) · {}",
            best,
            best - miss.champion_score,
            miss.source_score.against(best),
        ),
        // Absent, not zero. Inventing a `0.000000` best candidate would report
        // a measurement nobody took.
        None => "no candidate scored".to_string(),
    };
    format!(
        "🪢 Rebase not applied · {} {} from {} · champion {:.6} held · {best}",
        miss.attempted,
        enhancement_noun(miss.attempted),
        miss.source,
        miss.champion_score,
    )
}

/// Singular or plural, so the line reads as English at a count of one.
fn enhancement_noun(count: usize) -> &'static str {
    if count == 1 {
        "enhancement"
    } else {
        "enhancements"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp() -> RebaseStamp<'static> {
        RebaseStamp {
            score: 0.5,
            error: 0.5,
            champion_score: 0.4,
            source_score: SourceScore::Claimed(0.45),
            applied: 2,
            label: "bundle",
            source: "harvest",
        }
    }

    /// The `rebase` tag maps to 🪢 (NEAT-AI-Rebase #12). Pinned rather than
    /// left to the format string, because the sampler's commit subjects are
    /// keyed on the prefix and a silent change would be invisible until
    /// someone scanned the log looking for rebases and found none.
    #[test]
    fn both_messages_keep_the_knot_emoji() {
        let applied = rebase_message(&stamp());
        assert!(
            applied.starts_with("🪢 "),
            "the rebase tag is 🪢, got: {applied}"
        );
        assert!(
            !applied.contains('🔀'),
            "the old shuffle emoji must not come back: {applied}"
        );
        let held = no_improvement_message(&NoImprovement {
            champion_score: 0.5,
            best_score: Some(0.4),
            source_score: SourceScore::Claimed(0.45),
            attempted: 2,
            source: "harvest",
        });
        assert!(held.starts_with("🪢 "), "{held}");
    }

    #[test]
    fn every_delta_names_the_baseline_it_was_taken_from() {
        let m = rebase_message(&stamp());
        assert!(
            m.contains("champion 0.400000 → rebased 0.500000 (+1.00e-1)"),
            "{m}"
        );
        assert!(
            m.contains("claim delta +5.00e-2 vs claimed 0.450000"),
            "{m}"
        );
    }

    /// A rebased candidate routinely scores below the producer's own claim —
    /// the producer measured itself on an older, easier opening creature. That
    /// is a claim delta, not the creature getting worse, and it must never be
    /// rendered as `+-2.29e-6` either.
    #[test]
    fn a_candidate_below_the_claim_is_a_signed_claim_delta() {
        let m = rebase_message(&RebaseStamp {
            score: 0.4,
            source_score: SourceScore::Claimed(0.45),
            champion_score: 0.39,
            ..stamp()
        });
        assert!(
            m.contains("claim delta -5.00e-2 vs claimed 0.450000"),
            "{m}"
        );
        assert!(!m.contains("+-"), "a sign is never printed twice: {m}");
        assert!(!m.to_lowercase().contains("declin"), "{m}");
    }

    /// A source creature this run scored itself is not a claim, and saying
    /// "claimed" of it would misreport who measured it.
    #[test]
    fn a_validated_source_is_never_reported_as_a_claim() {
        let m = rebase_message(&RebaseStamp {
            source_score: SourceScore::Validated(0.45),
            ..stamp()
        });
        assert!(
            m.contains("source delta +5.00e-2 vs validated source 0.450000"),
            "{m}"
        );
        assert!(!m.contains("claim"), "{m}");

        let held = no_improvement_message(&NoImprovement {
            champion_score: 0.5,
            best_score: Some(0.4),
            source_score: SourceScore::Validated(0.45),
            attempted: 2,
            source: "harvest",
        });
        assert!(held.contains("vs validated source 0.450000"), "{held}");
        assert!(!held.contains("claim"), "{held}");
    }

    #[test]
    fn the_value_is_readable_whichever_way_it_was_measured() {
        assert!((SourceScore::Claimed(0.45).value() - 0.45).abs() < 1e-12);
        assert!((SourceScore::Validated(0.45).value() - 0.45).abs() < 1e-12);
    }

    #[test]
    fn one_enhancement_reads_as_singular_in_both_messages() {
        let m = rebase_message(&RebaseStamp {
            applied: 1,
            ..stamp()
        });
        assert!(m.contains("1 enhancement from harvest"), "{m}");
        let held = no_improvement_message(&NoImprovement {
            champion_score: 0.5,
            best_score: None,
            source_score: SourceScore::Claimed(0.45),
            attempted: 1,
            source: "harvest",
        });
        assert!(held.contains("1 enhancement from harvest"), "{held}");
        assert!(held.contains("no candidate scored"), "{held}");
    }
}
