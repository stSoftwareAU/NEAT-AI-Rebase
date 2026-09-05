//! A bundle file is untrusted input, and every walk over a patch tree recurses
//! (Issue #90).
//!
//! `Node` is a recursive tree, and `evaluate`, `is_finite`, `depth` and the
//! graft's emitter all descend it one stack frame per level. Nothing on the
//! forward path — the path that reads an incoming `--enhancements` bundle —
//! bounded that depth: the only bound in the codebase, `harvest`'s, guards the
//! *reverse* direction, reconstructing a patch out of a creature that already
//! carries it.
//!
//! These tests pin the forward direction. A patch nested deeper than
//! `MAX_PATCH_DEPTH` is refused at the parse boundary with a reason, and the
//! graft refuses it again for any patch that reaches it from somewhere other
//! than a parsed bundle — fail closed, the way every other malformed-patch case
//! here already does, rather than aborting the process on a stack overflow.

use std::path::{Path, PathBuf};

use neat_ai_rebase::cli::{Cli, EXIT_INCOMPATIBLE, run_with};
use neat_ai_rebase::compat::Incompatibility;
use neat_ai_rebase::corpus::corpus_info;
use neat_ai_rebase::enhancement::{
    Enhancement, EnhancementBundle, EnhancementError, Payload, ProducerContext,
};
use neat_ai_rebase::fixtures::evolved_descendant;
use neat_ai_rebase::patch::{Condition, MAX_PATCH_DEPTH, Node, Patch, Provenance};
use neat_core::creature_to_json;
use neat_core::training_data::TrainingDataConfig;

const PRODUCER: &str = "neat-ai-forests/test";

/// A left-leaning `split` chain `depth` levels deep, ending in leaves.
///
/// This is exactly the shape an attacker files: every level is a well-formed
/// `Split`, so nothing but a depth bound distinguishes it from a real patch.
fn deep_root(depth: usize) -> Node {
    let mut node = Node::leaf(0.5);
    for _ in 0..depth {
        node = Node::Split {
            condition: Condition::axis(0, 0.5),
            left: Box::new(Node::leaf(0.0)),
            right: Box::new(node),
        };
    }
    node
}

fn enhancement_with(root: Node, corpus_identity: &str) -> Enhancement {
    Enhancement::new(
        Payload::ForestPatch {
            patch: Patch::new(0, root, Provenance::default()),
        },
        &ProducerContext {
            producer: PRODUCER.into(),
            base_checksum: "opening-checksum".into(),
            base_score: 0.5,
            improved_score: 0.6,
            corpus_identity: corpus_identity.into(),
            input_count: 2,
            output_count: 1,
        },
    )
}

/// A champion, a corpus and an output directory, so the CLI runs for real.
struct Harness {
    _tmp: tempfile::TempDir,
    cli: Cli,
    corpus_identity: String,
    bundle_path: PathBuf,
}

impl Harness {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let training = tmp.path().join("training");
        std::fs::create_dir_all(&training).unwrap();
        let mut bytes = Vec::new();
        for record in 0..32u32 {
            for slot in 0..3 {
                bytes.extend_from_slice(&((record as f32) * 0.05 + slot as f32).to_le_bytes());
            }
        }
        std::fs::write(training.join("corpus.bin"), bytes).unwrap();
        let corpus = corpus_info(&training, &TrainingDataConfig::new(2, 1)).unwrap();

        let champion_path = tmp.path().join("champion.json");
        std::fs::write(
            &champion_path,
            creature_to_json(&evolved_descendant(2.0, 0.5)).unwrap(),
        )
        .unwrap();
        let bundle_path = tmp.path().join("enhancements.json");

        Self {
            cli: Cli {
                command: None,
                champion: Some(champion_path),
                enhancements: Some(bundle_path.clone()),
                harvest_from: None,
                screen_sample_rate: None,
                screen_held_out: false,
                training_data: Some(training),
                scorer: None,
                output_dir: Some(tmp.path().join("out")),
                scorer_args: Vec::new(),
                min_improvement: 1e-9,
                max_candidates: 8,
                // Nothing is scored here: the bundle is refused before a
                // candidate is ever built.
                dry_run: true,
            },
            corpus_identity: corpus.identity,
            bundle_path,
            _tmp: tmp,
        }
    }

    fn file(&self, enhancements: Vec<Enhancement>) {
        let bundle = EnhancementBundle::from_enhancements(enhancements);
        std::fs::write(&self.bundle_path, serde_json::to_string(&bundle).unwrap()).unwrap();
    }

    fn champion(&self) -> &Path {
        self.cli
            .champion
            .as_deref()
            .expect("the harness always writes a champion")
    }
}

/// The trigger from the issue, end to end over the real CLI: a bundle whose
/// `payload.patch.root` nests far deeper than any Forest tree.
#[test]
fn cli_refuses_a_bundle_whose_patch_tree_is_too_deep() {
    let h = Harness::new();
    h.file(vec![enhancement_with(
        deep_root(MAX_PATCH_DEPTH + 1),
        &h.corpus_identity,
    )]);

    let err = run_with(&h.cli, None).expect_err("a too-deep bundle is refused");

    assert_eq!(err.code, EXIT_INCOMPATIBLE);
    assert!(
        err.message.contains("nests deeper"),
        "the reason names the depth bound: {}",
        err.message
    );
}

/// The same run, with a tree at the bound rather than past it, still works —
/// the guard refuses depth, not patches.
#[test]
fn cli_accepts_a_bundle_at_the_depth_bound() {
    let h = Harness::new();
    h.file(vec![enhancement_with(
        deep_root(MAX_PATCH_DEPTH),
        &h.corpus_identity,
    )]);

    run_with(&h.cli, None).expect("a patch at the bound is still applied");
}

/// Both readers `load_one` can pick — bundle and bare enhancement — refuse it,
/// because either shape can be the file on disk.
#[test]
fn both_parsers_refuse_a_tree_past_the_bound() {
    let too_deep = enhancement_with(deep_root(MAX_PATCH_DEPTH + 1), "corpus-identity");
    let one = serde_json::to_string(&too_deep).unwrap();
    let bundle =
        serde_json::to_string(&EnhancementBundle::from_enhancements(vec![too_deep])).unwrap();

    for (shape, err) in [
        (
            "enhancement",
            Enhancement::parse_json(&one).expect_err("refused"),
        ),
        (
            "bundle",
            EnhancementBundle::parse_json(&bundle).expect_err("refused"),
        ),
    ] {
        match err {
            EnhancementError::Malformed(m) => assert!(
                m.contains("nests deeper"),
                "{shape} reason names the bound: {m}"
            ),
            other => panic!("{shape} refused for the wrong reason: {other}"),
        }
    }
}

/// A tree exactly at the bound parses, so the bound is off-by-one correct.
#[test]
fn a_tree_at_the_bound_parses() {
    let at_bound = enhancement_with(deep_root(MAX_PATCH_DEPTH), "corpus-identity");
    let text = serde_json::to_string(&at_bound).unwrap();

    assert_eq!(Enhancement::parse_json(&text).unwrap(), at_bound);
}

/// The graft is guarded in its own right: a patch that reaches it from
/// somewhere other than a parsed bundle is refused before `is_finite` or the
/// emitter walks it.
#[test]
fn the_graft_refuses_a_tree_past_the_bound() {
    let champion = evolved_descendant(2.0, 0.5);
    let patch = Patch::new(0, deep_root(MAX_PATCH_DEPTH + 1), Provenance::default());

    match neat_ai_rebase::forest::apply(&patch, &champion).expect_err("refused") {
        Incompatibility::Precondition(m) => {
            assert!(m.contains("nests deeper"), "reason names the bound: {m}");
        }
        other => panic!("refused for the wrong reason: {other}"),
    }
}

/// A document nested far past anything that could be built in memory fails
/// closed with a reason, rather than aborting the process.
#[test]
fn a_pathologically_nested_document_fails_closed() {
    const LEVELS: usize = 100_000;
    let mut text = String::from(
        r#"{"meta":{"version":1,"id":"0","producer":"x","baseChecksum":"c","baseScore":0.5,"improvedScore":0.6,"corpusIdentity":"i","inputCount":2,"outputCount":1},"payload":{"kind":"forestPatch","patch":{"version":1,"output":0,"root":"#,
    );
    for _ in 0..LEVELS {
        text.push_str(
            r#"{"kind":"split","condition":{"terms":[{"feature":0,"weight":1.0}],"threshold":0.5},"left":{"kind":"leaf","correction":0.0},"right":"#,
        );
    }
    text.push_str(r#"{"kind":"leaf","correction":0.5}"#);
    text.push_str(&"}".repeat(LEVELS));
    text.push_str("}}}");

    // The point is that this returns at all: a reason, not a SIGSEGV.
    let err = Enhancement::parse_json(&text).expect_err("a 100k-deep document is refused");
    assert!(matches!(err, EnhancementError::Malformed(_)), "{err}");
}

/// A bundle file the CLI reads from a directory is bounded too — the directory
/// reader funnels through the same parser.
#[test]
fn a_directory_member_is_bounded_as_well() {
    let h = Harness::new();
    let dir = h.champion().parent().unwrap().join("bundles");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle = EnhancementBundle::from_enhancements(vec![enhancement_with(
        deep_root(MAX_PATCH_DEPTH + 1),
        &h.corpus_identity,
    )]);
    std::fs::write(
        dir.join("00-forests.json"),
        serde_json::to_string(&bundle).unwrap(),
    )
    .unwrap();

    let mut cli = h.cli.clone();
    cli.enhancements = Some(dir);
    let err = run_with(&cli, None).expect_err("a too-deep directory member is refused");

    assert_eq!(err.code, EXIT_INCOMPATIBLE);
    assert!(err.message.contains("nests deeper"), "{}", err.message);
}
