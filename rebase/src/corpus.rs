//! Training-corpus identity.
//!
//! A score is only evidence about the corpus it was measured on, so every
//! enhancement carries the identity of that corpus and Rebase refuses to
//! replay one against a different corpus ([`crate::compat`]).
//!
//! The digest is the **fleet convention**, mirrored from NEAT-AI-Ockham,
//! NEAT-AI-Forests and NEAT-AI-Lamarck so all four agree on "same corpus"
//! without a shared crate: a 64-bit FNV-style mix over the widths, then per
//! file, its length, name, and first/last 64 bytes. Nothing reads the whole
//! corpus.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use neat_core::training_data::{TrainingDataConfig, find_bin_files};
use serde::{Deserialize, Serialize};

/// Deterministic identity of a corpus directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CorpusInfo {
    /// 64-bit FNV-style mix over widths, file names, sizes and head/tail bytes.
    pub identity: String,
    /// Total records across all `.bin` files.
    pub record_count: u64,
    /// Number of `.bin` files.
    pub file_count: usize,
    /// Input width used to interpret records.
    pub input_count: usize,
    /// Output width used to interpret records.
    pub output_count: usize,
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0100_0000_01b3;

fn mix(state: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *state ^= u64::from(*b);
        *state = state.wrapping_mul(FNV_PRIME);
    }
}

/// Compute the corpus identity and record count without reading every byte.
///
/// # Errors
///
/// Returns a message when the path is not a directory, holds no `.bin` files,
/// or holds a file whose length is not a whole number of records.
pub fn corpus_info(dir: &Path, config: &TrainingDataConfig) -> Result<CorpusInfo, String> {
    if !dir.is_dir() {
        return Err(format!(
            "training data path '{}' is not a directory",
            dir.display()
        ));
    }
    let files = find_bin_files(dir).map_err(|e| format!("cannot list '{}': {e}", dir.display()))?;
    if files.is_empty() {
        return Err(format!(
            "no .bin files found in training data directory '{}'",
            dir.display()
        ));
    }
    let record_bytes = config.bytes_per_record() as u64;
    let mut state = FNV_OFFSET;
    mix(&mut state, &(config.num_inputs as u64).to_le_bytes());
    mix(&mut state, &(config.num_outputs as u64).to_le_bytes());
    let mut records = 0u64;
    for path in &files {
        let mut file =
            File::open(path).map_err(|e| format!("cannot open '{}': {e}", path.display()))?;
        let len = file.metadata().map_err(|e| e.to_string())?.len();
        if len % record_bytes != 0 {
            return Err(format!(
                "'{}' is {len} bytes, not a multiple of the {record_bytes}-byte record size",
                path.display()
            ));
        }
        records += len / record_bytes;
        mix(&mut state, &len.to_le_bytes());
        if let Some(name) = path.file_name() {
            mix(&mut state, name.to_string_lossy().as_bytes());
        }
        let mut head = [0u8; 64];
        let n = read_up_to(&mut file, &mut head)?;
        mix(&mut state, &head[..n]);
        if len > 64 {
            file.seek(SeekFrom::End(-64)).map_err(|e| e.to_string())?;
            let mut tail = [0u8; 64];
            let n = read_up_to(&mut file, &mut tail)?;
            mix(&mut state, &tail[..n]);
        }
    }
    Ok(CorpusInfo {
        identity: format!("{state:016x}"),
        record_count: records,
        file_count: files.len(),
        input_count: config.num_inputs,
        output_count: config.num_outputs,
    })
}

fn read_up_to(file: &mut File, buf: &mut [u8]) -> Result<usize, String> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = file.read(&mut buf[filled..]).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_corpus(dir: &Path, name: &str, records: &[f32]) {
        let mut f = File::create(dir.join(name)).unwrap();
        for v in records {
            f.write_all(&v.to_le_bytes()).unwrap();
        }
    }

    #[test]
    fn identity_is_stable_and_moves_with_the_data() {
        let tmp = tempfile::tempdir().unwrap();
        write_corpus(tmp.path(), "a.bin", &[0.0, 1.0, 2.0, 3.0]);
        let config = TrainingDataConfig::new(1, 1);
        let first = corpus_info(tmp.path(), &config).unwrap();
        assert_eq!(first.record_count, 2);
        assert_eq!(first.file_count, 1);
        assert_eq!(corpus_info(tmp.path(), &config).unwrap(), first);

        write_corpus(tmp.path(), "a.bin", &[0.0, 1.0, 2.0, 9.0]);
        assert_ne!(
            corpus_info(tmp.path(), &config).unwrap().identity,
            first.identity
        );
    }

    #[test]
    fn widths_participate_in_the_identity() {
        let tmp = tempfile::tempdir().unwrap();
        write_corpus(tmp.path(), "a.bin", &[0.0, 1.0, 2.0, 3.0]);
        let one = corpus_info(tmp.path(), &TrainingDataConfig::new(1, 1)).unwrap();
        let three = corpus_info(tmp.path(), &TrainingDataConfig::new(3, 1)).unwrap();
        assert_ne!(one.identity, three.identity);
    }

    #[test]
    fn a_ragged_file_fails_closed() {
        let tmp = tempfile::tempdir().unwrap();
        write_corpus(tmp.path(), "a.bin", &[0.0, 1.0, 2.0]);
        let err = corpus_info(tmp.path(), &TrainingDataConfig::new(1, 1)).unwrap_err();
        assert!(err.contains("not a multiple"), "{err}");
    }

    #[test]
    fn an_empty_or_missing_directory_fails_closed() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(
            corpus_info(tmp.path(), &TrainingDataConfig::new(1, 1))
                .unwrap_err()
                .contains("no .bin files")
        );
        assert!(
            corpus_info(&tmp.path().join("nope"), &TrainingDataConfig::new(1, 1))
                .unwrap_err()
                .contains("not a directory")
        );
    }
}
