//! Creature JSON tags, preserved across a rebase.
//!
//! `neat_core::CreatureExport` does not round-trip `tags`: it parses the
//! fields it models and drops the rest. That is correct for a validation
//! contract and wrong for anything that writes a creature back to a
//! population — GRQ-sampler's check-in guard refuses a creature that arrived
//! with a better score but lost its discovery and intelligent-design
//! provenance, and it is right to.
//!
//! Forests, Lamarck and Ockham all solve this the same way: keep the tags in a
//! sidecar parsed from the original JSON and re-attach them on write. Rebase
//! does the same, with one difference that matters — a rebased creature has
//! **two** parents. The tags come from the champion, because that is the
//! creature being improved and the lineage the population is tracking; what
//! the Forests side contributed is recorded in the `rebase` tag rather than by
//! overwriting the champion's own provenance.
//!
//! Deliberately dropped on serialise, following Ockham: creature-level `uuid`
//! and `memetic`. The structure changed, so either would be a lie.

use std::collections::{BTreeMap, HashSet};

use neat_core::{CreatureExport, creature_to_json};
use serde_json::{Map, Value};

/// One `{ name, value }` tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    /// Tag key (`score`, `error`, `forests`, `rebase`, …).
    pub name: String,
    /// Tag value; always a string in the export format.
    pub value: String,
}

impl Tag {
    /// Convenience constructor.
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

fn parse_tags(v: Option<&Value>) -> Vec<Tag> {
    let mut out = Vec::new();
    if let Some(Value::Array(tags)) = v {
        for t in tags {
            if let Value::Object(o) = t
                && let Some(Value::String(name)) = o.get("name")
            {
                let value = match o.get("value") {
                    Some(Value::String(s)) => s.clone(),
                    Some(v) => v.to_string(),
                    None => String::new(),
                };
                out.push(Tag {
                    name: name.clone(),
                    value,
                });
            }
        }
    }
    out
}

fn tags_value(tags: &[Tag]) -> Value {
    Value::Array(
        tags.iter()
            .map(|t| serde_json::json!({"name": t.name, "value": t.value}))
            .collect(),
    )
}

/// Tags carried alongside a creature.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CreatureMeta {
    /// Creature-level tags, in their original order.
    pub tags: Vec<Tag>,
    /// Per-neuron tags keyed by neuron uuid.
    pub neuron_tags: BTreeMap<String, Vec<Tag>>,
}

impl CreatureMeta {
    /// Parse creature-level and per-neuron tags from raw creature JSON.
    ///
    /// Malformed or absent tags yield an empty sidecar rather than an error:
    /// a creature without tags is perfectly valid, it just has nothing to
    /// preserve.
    pub fn from_json(text: &str) -> Self {
        let mut out = Self::default();
        if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(text) {
            out.tags = parse_tags(map.get("tags"));
            if let Some(Value::Array(neurons)) = map.get("neurons") {
                for n in neurons {
                    if let Value::Object(o) = n
                        && let Some(Value::String(uuid)) = o.get("uuid")
                    {
                        let tags = parse_tags(o.get("tags"));
                        if !tags.is_empty() {
                            out.neuron_tags.insert(uuid.clone(), tags);
                        }
                    }
                }
            }
        }
        out
    }

    /// Read one creature-level tag.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.tags
            .iter()
            .find(|t| t.name == name)
            .map(|t| t.value.as_str())
    }

    /// Read the `score` tag as a number, when it is one.
    ///
    /// This is the fleet's own claim about the creature. Treat it as a label
    /// for choosing what to work on, never as a measurement to compare against
    /// a score this run produced: a tag was written by another host, on
    /// another day, possibly against another corpus.
    pub fn score(&self) -> Option<f64> {
        self.get("score").and_then(|v| v.parse().ok())
    }

    /// Replace or append a creature-level tag, keeping its original position.
    pub fn upsert(&mut self, name: &str, value: impl Into<String>) {
        let value = value.into();
        if let Some(t) = self.tags.iter_mut().find(|t| t.name == name) {
            t.value = value;
        } else {
            self.tags.push(Tag {
                name: name.into(),
                value,
            });
        }
    }

    /// Drop per-neuron tags whose uuid is no longer in `creature`.
    ///
    /// A rebase adds neurons and an Ockham replay removes them, so the sidecar
    /// has to be reconciled before it is written back or it would name
    /// neurons that no longer exist.
    pub fn retain_neurons(&mut self, creature: &CreatureExport) {
        let keep: HashSet<&str> = creature.neurons.iter().map(|n| n.uuid.as_str()).collect();
        self.neuron_tags
            .retain(|uuid, _| keep.contains(uuid.as_str()));
    }

    /// Stamp the authoritative score, error and a `rebase` summary.
    pub fn stamp(&mut self, outcome: &RebaseStamp<'_>) {
        self.upsert("score", format!("{}", outcome.score));
        self.upsert("error", format!("{}", outcome.error));
        self.upsert("rebase", rebase_message(outcome));
    }

    /// Serialise `creature` with the creature-level and remaining per-neuron
    /// tags re-attached.
    ///
    /// # Errors
    ///
    /// The serialisation failure when the creature or the assembled document
    /// cannot be written.
    pub fn serialize_with(
        &self,
        creature: &CreatureExport,
        pretty: bool,
    ) -> Result<String, String> {
        let text = creature_to_json(creature).map_err(|e| e.to_string())?;
        let mut value: Map<String, Value> =
            serde_json::from_str(&text).map_err(|e| e.to_string())?;
        if !self.tags.is_empty() {
            value.insert("tags".into(), tags_value(&self.tags));
        }
        if !self.neuron_tags.is_empty()
            && let Some(Value::Array(neurons)) = value.get_mut("neurons")
        {
            for n in neurons.iter_mut() {
                if let Value::Object(o) = n
                    && let Some(Value::String(uuid)) = o.get("uuid")
                    && let Some(tags) = self.neuron_tags.get(uuid)
                {
                    o.insert("tags".into(), tags_value(tags));
                }
            }
        }
        let v = Value::Object(value);
        if pretty {
            serde_json::to_string_pretty(&v)
        } else {
            serde_json::to_string(&v)
        }
        .map_err(|e| e.to_string())
    }
}

/// What the `rebase` tag reports about a promoted candidate.
#[derive(Debug, Clone, Copy)]
pub struct RebaseStamp<'a> {
    /// Authoritative score of the promoted candidate.
    pub score: f64,
    /// Authoritative error of the promoted candidate.
    pub error: f64,
    /// Score of the champion it was built on, from the same scorer call.
    pub champion_score: f64,
    /// Score of the creature whose discoveries were rebased, same call.
    pub source_score: f64,
    /// How many enhancements the promoted candidate applied.
    pub applied: usize,
    /// Which cohort member won (`bundle`, `single-02`, …).
    pub label: &'a str,
    /// Where the enhancements came from (`neat-ai-forests`, `harvest`, …).
    pub source: &'a str,
}

/// GRQ-sampler skim line; becomes the sampler commit subject.
///
/// It names both numbers a reader needs to see that the rebase was worth
/// doing: how far the promoted creature beat the champion it was built on,
/// and how far it beat the creature whose discoveries it borrowed. The second
/// is the one that says publishing that creature alone would have been a loss.
pub fn rebase_message(stamp: &RebaseStamp<'_>) -> String {
    format!(
        "🔀 Rebase · {} {} from {} · score: {:.6} (+{:.2e} vs champion, +{:.2e} vs source)",
        stamp.applied,
        if stamp.applied == 1 {
            "enhancement"
        } else {
            "enhancements"
        },
        stamp.source,
        stamp.score,
        stamp.score - stamp.champion_score,
        stamp.score - stamp.source_score,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::linear_hidden_creature;

    fn tagged_json() -> String {
        let creature = linear_hidden_creature(2.0);
        let text = creature_to_json(&creature).unwrap();
        let mut v: Map<String, Value> = serde_json::from_str(&text).unwrap();
        v.insert(
            "tags".into(),
            serde_json::json!([
                {"name": "error", "value": "0.6027"},
                {"name": "score", "value": "0.3964"},
                {"name": "name", "value": "Frank Bailey"},
                {"name": "forests", "value": "🌳 Forests · 4 accepts"},
            ]),
        );
        if let Some(Value::Array(neurons)) = v.get_mut("neurons")
            && let Value::Object(o) = &mut neurons[0]
        {
            o.insert(
                "tags".into(),
                serde_json::json!([{"name": "origin", "value": "evolution"}]),
            );
        }
        serde_json::to_string_pretty(&Value::Object(v)).unwrap()
    }

    #[test]
    fn tags_survive_a_parse_and_write() {
        let text = tagged_json();
        let meta = CreatureMeta::from_json(&text);
        assert_eq!(meta.tags.len(), 4);
        assert_eq!(meta.get("name"), Some("Frank Bailey"));
        assert_eq!(meta.score(), Some(0.3964));
        assert_eq!(meta.neuron_tags.len(), 1);

        let creature = neat_core::parse_creature_json(&text).unwrap();
        let written = meta.serialize_with(&creature, true).unwrap();
        let round = CreatureMeta::from_json(&written);
        assert_eq!(round, meta, "tags must survive the round trip intact");
    }

    #[test]
    fn a_creature_without_tags_is_not_an_error() {
        let creature = linear_hidden_creature(2.0);
        let text = creature_to_json(&creature).unwrap();
        let meta = CreatureMeta::from_json(&text);
        assert!(meta.tags.is_empty());
        assert_eq!(meta.score(), None);
        // And writing it back adds no empty `tags` key.
        let written = meta.serialize_with(&creature, false).unwrap();
        assert!(!written.contains("\"tags\""), "{written}");
    }

    #[test]
    fn stamping_updates_score_and_error_in_place_and_appends_rebase() {
        let mut meta = CreatureMeta::from_json(&tagged_json());
        let before: Vec<String> = meta.tags.iter().map(|t| t.name.clone()).collect();
        meta.stamp(&RebaseStamp {
            score: 0.3965,
            error: 0.6035,
            champion_score: 0.39644,
            source_score: 0.396463,
            applied: 4,
            label: "bundle",
            source: "harvest",
        });
        // score/error replaced in place, provenance untouched, `rebase` appended.
        assert_eq!(meta.get("score"), Some("0.3965"));
        assert_eq!(meta.get("error"), Some("0.6035"));
        assert_eq!(meta.get("name"), Some("Frank Bailey"));
        assert_eq!(meta.get("forests"), Some("🌳 Forests · 4 accepts"));
        let after: Vec<String> = meta.tags.iter().map(|t| t.name.clone()).collect();
        assert_eq!(&after[..before.len()], &before[..], "order is preserved");
        assert_eq!(after.last().unwrap(), "rebase");
        let message = meta.get("rebase").unwrap();
        assert!(
            message.starts_with("🔀 Rebase · 4 enhancements from harvest"),
            "{message}"
        );
        assert!(message.contains("vs source"), "{message}");
    }

    #[test]
    fn neuron_tags_for_removed_neurons_are_dropped() {
        let text = tagged_json();
        let mut meta = CreatureMeta::from_json(&text);
        let mut creature = neat_core::parse_creature_json(&text).unwrap();
        assert_eq!(meta.neuron_tags.len(), 1);
        // An Ockham replay removed the tagged neuron.
        creature.neurons.retain(|n| n.uuid != "h1");
        meta.retain_neurons(&creature);
        assert!(meta.neuron_tags.is_empty());
    }

    #[test]
    fn the_message_names_both_comparisons() {
        let m = rebase_message(&RebaseStamp {
            score: 0.5,
            error: 0.5,
            champion_score: 0.4,
            source_score: 0.45,
            applied: 1,
            label: "single-00",
            source: "neat-ai-forests",
        });
        assert!(m.contains("1 enhancement from neat-ai-forests"), "{m}");
        assert!(m.contains("vs champion"), "{m}");
        assert!(m.contains("vs source"), "{m}");
    }
}
