//! Portable production history and transactional official reference updates.
mod observation;
mod storage;

pub(crate) use observation::{Fingerprint, Outcome};
pub use observation::{History, Observation, Profile};

use crate::predict::Database;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io,
    path::{Path, PathBuf},
};

const SCHEMA: u32 = 1;
const BUNDLED_VERSION: u64 = 1;
const MIN_SAMPLES: usize = 3;
const MAX_SAMPLES: usize = 4096;
const MAX_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug)]
pub struct Store {
    official: PathBuf,
    local: PathBuf,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Release {
    schema: u32,
    version: u64,
    dataset: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Installed {
    sha256: String,
    package: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Local {
    schema: u32,
    records: Vec<Observation>,
}

fn invalid(reason: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

fn decode<T: serde::de::DeserializeOwned>(data: &[u8]) -> io::Result<T> {
    serde_json::from_slice(data).map_err(io::Error::other)
}

fn verify(package: &str, expected: &str) -> io::Result<(u64, Database)> {
    if format!("{:x}", Sha256::digest(package.as_bytes())) != expected {
        return Err(invalid("official package SHA-256 mismatch"));
    }
    let release: Release = decode(package.as_bytes())?;
    if release.schema != SCHEMA || release.version <= BUNDLED_VERSION {
        return Err(invalid("unsupported official schema or version"));
    }
    Ok((release.version, Database::from_official(&release.dataset)?))
}

impl Store {
    /// The caller supplies the canonical EXE directory, or an isolated test root.
    /// Opening the handle performs no filesystem work.
    pub fn portable(root: &Path) -> io::Result<Self> {
        if !root.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "absolute portable root required",
            ));
        }
        Ok(Self {
            official: root.join("data").join("official"),
            local: root.join("data").join("local"),
        })
    }

    /// Verifies a downloaded package against a digest from trusted release
    /// metadata. A digest carried by the download itself is insufficient.
    pub fn install_official(&self, package: &str, expected_sha256: &str) -> io::Result<u64> {
        if package.len() as u64 > MAX_BYTES {
            return Err(invalid("official package too large"));
        }
        let (version, _) = verify(package, expected_sha256)?;
        let _lock = storage::lock(&self.official)?;
        if version <= self.official()?.0 {
            return Err(invalid("official update must increase the version"));
        }
        let installed = Installed {
            sha256: expected_sha256.into(),
            package: package.into(),
        };
        storage::replace(&self.official, "active.json", &installed)?;
        Ok(version)
    }

    pub fn official(&self) -> io::Result<(u64, Database)> {
        let Some(data) = storage::read(&self.official.join("active.json"))? else {
            return Ok((BUNDLED_VERSION, Database::bundled()?));
        };
        let installed: Installed = decode(&data)?;
        verify(&installed.package, &installed.sha256)
    }

    fn local(&self) -> io::Result<Local> {
        let Some(data) = storage::read(&self.local.join("production-v1.json"))? else {
            return Ok(Local {
                schema: SCHEMA,
                records: Vec::new(),
            });
        };
        let local: Local = decode(&data)?;
        if local.schema != SCHEMA
            || local.records.len() > MAX_SAMPLES
            || !local.records.iter().all(Observation::valid)
        {
            return Err(invalid("invalid production history"));
        }
        Ok(local)
    }

    pub(crate) fn record(&self, observation: Observation) -> io::Result<()> {
        if !observation.valid() {
            return Err(invalid("invalid production observation"));
        }
        let _lock = storage::lock(&self.local)?;
        let mut local = self.local()?;
        if local.records.len() == MAX_SAMPLES {
            local.records.remove(0);
        }
        local.records.push(observation);
        storage::replace(&self.local, "production-v1.json", &local)
    }

    /// Descriptive history for exact matches. Independent calibration is
    /// still required for any production ETA or proactive software decision.
    pub fn history(&self, profile: &Profile) -> io::Result<Option<History>> {
        if !profile.complete() {
            return Ok(None);
        }
        let mut history = History::default();
        for sample in self
            .local()?
            .records
            .iter()
            .filter(|s| s.profile() == profile)
        {
            history.include(sample);
        }
        Ok((history.samples >= MIN_SAMPLES).then_some(history))
    }
}

#[cfg(test)]
mod tests;
