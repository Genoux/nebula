//! The recorded wake phrase on disk.
//!
//! A hand-rolled binary format rather than a serialisation crate: this is a
//! magic number, four integers and a block of little-endian f32, and the file
//! is written by one program and read by the same one.

use anyhow::{Context, Result, bail};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::mfcc::COEFFS;

const MAGIC: &[u8; 4] = b"NBWW";
const VERSION: u32 = 1;
const MAX_BYTES: u64 = 2 * 1024 * 1024;

/// Several recordings of the same phrase. More templates means a better chance
/// that one of them resembles how the phrase was said this time.
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    pub phrase: String,
    pub templates: Vec<Vec<Vec<f32>>>,
    /// How far apart the user's own recordings are from each other. A live
    /// utterance is scored against this rather than against a fixed constant,
    /// so the same sensitivity means the same thing for every voice and every
    /// microphone.
    pub reference: f32,
}

impl Model {
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("could not create {}", parent.display()))?;
        }
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());

        let phrase = self.phrase.as_bytes();
        out.extend_from_slice(&(phrase.len() as u32).to_le_bytes());
        out.extend_from_slice(phrase);
        out.extend_from_slice(&self.reference.to_le_bytes());

        out.extend_from_slice(&(self.templates.len() as u32).to_le_bytes());
        for template in &self.templates {
            out.extend_from_slice(&(template.len() as u32).to_le_bytes());
            for frame in template {
                for coefficient in frame {
                    out.extend_from_slice(&coefficient.to_le_bytes());
                }
            }
        }
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("could not write {}", path.display()))?
            .write_all(&out)?;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if !matches!(
            self.phrase.trim().to_lowercase().as_str(),
            "nebula" | "hey nebula"
        ) || !(2..=32).contains(&self.templates.len())
            || !self.reference.is_finite()
            || self.reference <= 0.0
            || self.templates.iter().any(|template| {
                template.is_empty()
                    || template.len() > 160
                    || template
                        .iter()
                        .any(|frame| frame.len() != COEFFS || frame.iter().any(|v| !v.is_finite()))
            })
        {
            bail!("invalid wake model; retrain with --train-wake");
        }
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .with_context(|| {
                format!(
                    "no wake model at {} - record one with --train-wake",
                    path.display()
                )
            })?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BYTES {
            bail!("wake model exceeds size limit");
        }
        Self::decode(&bytes).with_context(|| format!("{} is not a wake model", path.display()))
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut at = 0usize;
        let mut take = |n: usize| -> Result<&[u8]> {
            let end = at + n;
            if end > bytes.len() {
                bail!("file ends in the middle of a record");
            }
            let slice = &bytes[at..end];
            at = end;
            Ok(slice)
        };

        if take(4)? != MAGIC {
            bail!("wrong magic number");
        }
        let version = u32::from_le_bytes(take(4)?.try_into()?);
        if version != VERSION {
            bail!("model version {version}, but this build reads version {VERSION}");
        }

        let phrase_len = u32::from_le_bytes(take(4)?.try_into()?) as usize;
        let phrase = String::from_utf8(take(phrase_len)?.to_vec())?;
        let reference = f32::from_le_bytes(take(4)?.try_into()?);

        let count = u32::from_le_bytes(take(4)?.try_into()?) as usize;
        if !(2..=32).contains(&count) {
            bail!("wake model needs 2..=32 templates");
        }
        let mut templates = Vec::with_capacity(count);
        for _ in 0..count {
            let frames = u32::from_le_bytes(take(4)?.try_into()?) as usize;
            if !(1..=160).contains(&frames) {
                bail!("invalid wake template length");
            }
            let mut template = Vec::with_capacity(frames);
            for _ in 0..frames {
                let raw = take(COEFFS * 4)?;
                template.push(
                    raw.chunks_exact(4)
                        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
                        .collect(),
                );
            }
            templates.push(template);
        }
        let model = Self {
            phrase,
            templates,
            reference,
        };
        model.validate()?;
        Ok(model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> Model {
        Model {
            phrase: "nebula".into(),
            templates: vec![
                vec![vec![0.5; COEFFS], vec![-1.25; COEFFS]],
                vec![vec![2.0; COEFFS]],
            ],
            reference: 3.5,
        }
    }

    #[test]
    fn a_model_survives_a_round_trip_through_disk() {
        let path = std::env::temp_dir().join("nebula-wake-model-test.bin");
        let original = model();
        original.save(&path).unwrap();
        assert_eq!(Model::load(&path).unwrap(), original);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn invalid_models_fail_before_being_written() {
        let mut model = model();
        model.reference = f32::NAN;
        assert!(model.validate().is_err());
        model.reference = 1.0;
        model.templates[0][0][0] = f32::INFINITY;
        assert!(model.validate().is_err());
        model.templates.clear();
        assert!(model.validate().is_err());
    }

    #[test]
    fn a_corrupt_template_count_is_rejected_before_allocation() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&6u32.to_le_bytes());
        bytes.extend_from_slice(b"nebula");
        bytes.extend_from_slice(&1.0f32.to_le_bytes());
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(Model::decode(&bytes).is_err());
    }

    #[test]
    fn a_foreign_file_is_rejected() {
        assert!(Model::decode(b"not a model at all").is_err());
    }

    #[test]
    fn a_truncated_file_is_an_error_rather_than_a_panic() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&6u32.to_le_bytes());
        assert!(Model::decode(&bytes).is_err());
    }

    #[test]
    fn a_future_version_is_refused_by_name() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&99u32.to_le_bytes());
        let err = format!("{:#}", Model::decode(&bytes).unwrap_err());
        assert!(err.contains("version 99"), "{err}");
    }

    #[test]
    fn a_missing_file_says_how_to_make_one() {
        let err = format!(
            "{:#}",
            Model::load(Path::new("/nonexistent/wake.bin")).unwrap_err()
        );
        assert!(err.contains("--train-wake"), "{err}");
    }
}
