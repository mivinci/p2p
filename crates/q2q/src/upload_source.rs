//! Content available for upload, independent of publication and transport.
use crate::error::TransferError;
use crate::source_file::PreparedFile;
use q2q_core::{merkle::Proof, seed::Seed};
use std::sync::Arc;
use tokio::sync::{Semaphore, watch};

#[derive(Clone)]
pub(crate) struct Availability {
  pub(crate) bits: Vec<u8>,
  pub(crate) failed: bool,
  pub(crate) complete: bool,
}
impl Availability {
  pub(crate) fn has(&self, index: usize) -> bool {
    !self.failed
      && self
        .bits
        .get(index / 8)
        .is_some_and(|byte| byte & (1 << (7 - index % 8)) != 0)
  }
  pub(crate) fn any(&self) -> bool {
    !self.failed && self.bits.iter().any(|byte| *byte != 0)
  }
}

/// Revisions use opened files, never caller-controlled paths. Download sources
/// include the receipt file so proof-only external changes also invalidate hits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SourceRevision {
  data: FileRevision,
  receipts: Option<FileRevision>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct FileRevision {
  length: u64,
  modified: Option<std::time::SystemTime>,
  #[cfg(unix)]
  changed: (i64, i64),
}
impl FileRevision {
  fn from_metadata(metadata: &std::fs::Metadata) -> Self {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    Self {
      length: metadata.len(),
      modified: metadata.modified().ok(),
      #[cfg(unix)]
      changed: (metadata.ctime(), metadata.ctime_nsec()),
    }
  }
}
impl SourceRevision {
  pub(crate) fn from_metadata(metadata: &std::fs::Metadata) -> Self {
    Self {
      data: FileRevision::from_metadata(metadata),
      receipts: None,
    }
  }
  pub(crate) fn with_receipts(mut self, metadata: &std::fs::Metadata) -> Self {
    self.receipts = Some(FileRevision::from_metadata(metadata));
    self
  }
}

pub(crate) trait UploadSource: Send + Sync {
  fn seed(&self) -> &Seed;
  fn availability(&self) -> watch::Receiver<Availability>;
  fn read_limit(&self) -> Arc<Semaphore>;
  /// Blocking availability/metadata check. A revision change invalidates the
  /// cached snapshot; only a fresh verified read may populate the new revision.
  fn revision(&self, index: usize) -> Result<SourceRevision, TransferError>;
  /// Runs on a blocking thread with a read permit. Data and proof must refer
  /// to the same verified block; sparse or merely received bytes are excluded.
  fn read_block(&self, index: usize) -> Result<(Vec<u8>, Proof), TransferError>;
}
impl UploadSource for PreparedFile {
  fn seed(&self) -> &Seed {
    self.seed()
  }
  fn availability(&self) -> watch::Receiver<Availability> {
    let count = self.seed().info.leaf_count();
    let mut bits = vec![0; count.div_ceil(8)];
    for index in 0..count {
      bits[index / 8] |= 1 << (7 - index % 8);
    }
    watch::channel(Availability {
      bits,
      failed: false,
      complete: true,
    })
    .1
  }
  fn read_limit(&self) -> Arc<Semaphore> {
    self.read_limit()
  }
  fn revision(&self, index: usize) -> Result<SourceRevision, TransferError> {
    if index >= self.block_count() {
      return Err(crate::source_file::PrepareError::BlockOutOfRange(index).into());
    }
    Ok(self.source_revision()?)
  }
  fn read_block(&self, index: usize) -> Result<(Vec<u8>, Proof), TransferError> {
    Ok((self.read_verified_block(index)?, self.proof(index)?))
  }
}

#[cfg(test)]
#[path = "../tests/upload_source.rs"]
mod tests;
