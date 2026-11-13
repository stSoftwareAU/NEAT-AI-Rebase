//! Experimental scorer: bounded execution and a complete measurement contract.

use neat_ai_rebase::scorer::{ScoreResult, ScorerError, parse_scorer_output};
use std::collections::BTreeMap;

pub(crate) fn parse(text: &str) -> Result<BTreeMap<String, ScoreResult>, ScorerError> {
    let raw: serde_json::Value =
        serde_json::from_str(text).map_err(|e| ScorerError::Malformed(e.to_string()))?;
    let entries = raw
        .as_object()
        .ok_or_else(|| ScorerError::Malformed("expected result object".into()))?;
    for (label, result) in entries {
        for field in ["score", "error", "complexityPenalty", "timeTaken"] {
            if result
                .get(field)
                .and_then(|v| v.as_f64())
                .is_none_or(|v| !v.is_finite())
            {
                return Err(ScorerError::Malformed(format!(
                    "{label}: missing or invalid {field}"
                )));
            }
        }
        if result
            .get("recordCount")
            .and_then(|v| v.as_u64())
            .is_none_or(|n| n == 0)
        {
            return Err(ScorerError::Malformed(format!(
                "{label}: missing or invalid recordCount"
            )));
        }
    }
    parse_scorer_output(text)
}

#[cfg(not(test))]
pub(crate) struct BoundedScorer {
    binary: std::path::PathBuf,
    args: Vec<String>,
    timeout: String,
    seconds: u32,
    checksum: String,
}

#[cfg(not(test))]
impl BoundedScorer {
    pub(crate) fn from_manifest(path: &std::path::Path) -> Result<Self, String> {
        let manifest = super::manifest::Manifest::load(path)?;
        let timeout = ["timeout", "gtimeout"]
            .into_iter()
            .find(|candidate| {
                std::process::Command::new(candidate)
                    .arg("--version")
                    .output()
                    .is_ok_and(|out| out.status.success())
            })
            .ok_or("a bounded experiment requires timeout or gtimeout")?;
        Ok(Self {
            binary: manifest.scorer.path,
            args: manifest.protocol.scorer_args,
            timeout: timeout.into(),
            seconds: manifest.protocol.evaluation_timeout_seconds,
            checksum: manifest.scorer.sha256,
        })
    }
}

#[cfg(not(test))]
impl neat_ai_rebase::scorer::DirectoryScorer for BoundedScorer {
    fn score_directory(
        &self,
        creatures: &std::path::Path,
        training: &std::path::Path,
        mode: neat_ai_rebase::scorer::ScorerMode,
    ) -> Result<BTreeMap<String, ScoreResult>, ScorerError> {
        if !mode.is_authoritative() {
            return Err(ScorerError::NotAuthoritative(mode.label()));
        }
        let out = std::process::Command::new(&self.timeout)
            .args(["--signal=TERM", "--kill-after=5", &self.seconds.to_string()])
            .arg(&self.binary)
            .args(&self.args)
            .arg(creatures)
            .arg(training)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| ScorerError::Spawn(e.to_string()))?;
        if !out.status.success() {
            return Err(ScorerError::Failed {
                status: out.status.to_string(),
                stderr: String::from_utf8_lossy(&out.stderr)
                    .chars()
                    .take(4096)
                    .collect(),
            });
        }
        parse(&String::from_utf8_lossy(&out.stdout))
    }

    fn identity(&self) -> String {
        format!("sha256:{}", self.checksum)
    }
}
