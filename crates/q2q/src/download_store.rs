//! Root-bound, crash-recoverable block receipts for SDK downloads.

use crate::error::TransferError;
use q2q_core::block::PIECE_SIZE;
use q2q_core::file::{FileStore, StoreError};
use q2q_core::merkle::{Hash, Proof};
use q2q_core::peer_wire::MAX_CONTROL_BYTES;
use q2q_core::seed::Seed;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
#[cfg(test)]
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
  version: u32,
  info_hash: [u8; 32],
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
  index: usize,
  proof: Proof,
}

pub(crate) struct DownloadStore {
  store: FileStore,
  // The OS releases this advisory lock on process exit. Blocking writes retain
  // the enclosing Arc, so cancellation cannot release it during a disk write.
  destination: File,
  journal: File,
  failed: bool,
  receipts: Vec<Option<u64>>,
}

#[cfg(test)]
impl Deref for DownloadStore {
  type Target = FileStore;
  fn deref(&self) -> &FileStore {
    &self.store
  }
}
#[cfg(test)]
impl DerefMut for DownloadStore {
  fn deref_mut(&mut self) -> &mut FileStore {
    &mut self.store
  }
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
  let mut name = path.as_os_str().to_os_string();
  name.push(suffix);
  name.into()
}

fn invalid(message: impl Into<String>) -> TransferError {
  TransferError::Checkpoint(message.into())
}

fn write_record(file: &mut File, value: &impl Serialize) -> Result<(), TransferError> {
  let mut bytes = serde_json::to_vec(value).map_err(|e| invalid(e.to_string()))?;
  if bytes.len() > MAX_CONTROL_BYTES {
    return Err(invalid("checkpoint record exceeds size limit"));
  }
  bytes.push(b'\n');
  file.write_all(&bytes)?;
  Ok(())
}

// Read one bounded record. A final torn append is discarded on resume.
fn read_record(reader: &mut impl BufRead) -> Result<Option<Vec<u8>>, TransferError> {
  let mut line = Vec::new();
  reader
    .take((MAX_CONTROL_BYTES + 2) as u64)
    .read_until(b'\n', &mut line)?;
  if line.len() > MAX_CONTROL_BYTES + 1 {
    return Err(invalid("checkpoint record exceeds size limit"));
  }
  if line.last() != Some(&b'\n') {
    return Ok(None);
  }
  line.pop();
  Ok(Some(line))
}

fn sync_parent(_path: &Path) -> Result<(), TransferError> {
  #[cfg(unix)]
  File::open(
    _path
      .parent()
      .filter(|p| !p.as_os_str().is_empty())
      .unwrap_or(Path::new(".")),
  )?
  .sync_all()?;
  Ok(())
}

struct Temporary(PathBuf);
impl Drop for Temporary {
  fn drop(&mut self) {
    let _ = std::fs::remove_file(&self.0);
  }
}

impl DownloadStore {
  pub(crate) fn verified_bitfield(&self) -> Vec<u8> {
    self.store.verified_bitfield()
  }
  pub(crate) fn complete(&self) -> Result<bool, TransferError> {
    for index in 0..self.store.block_count() {
      if self.store.state(index)? != q2q_core::block::BlockState::Verified {
        return Ok(false);
      }
    }
    Ok(true)
  }
  pub(crate) fn read_verified(&self, index: usize) -> Result<Vec<u8>, TransferError> {
    if self.store.state(index)? != q2q_core::block::BlockState::Verified {
      return Err(std::io::Error::other("block is not verified").into());
    }
    let bytes = self.store.read_block(index)?;
    if self.store.cache().leaf(index) != Some(&q2q_core::merkle::leaf_hash(&bytes)) {
      return Err(
        std::io::Error::new(
          std::io::ErrorKind::InvalidData,
          "verified destination block changed",
        )
        .into(),
      );
    }
    Ok(bytes)
  }
  pub(crate) fn upload_revision(
    &self,
    index: usize,
  ) -> Result<crate::upload_source::SourceRevision, TransferError> {
    if self.failed || self.receipts.get(index).copied().flatten().is_none() {
      return Err(invalid("block is unavailable for sharing"));
    }
    Ok(
      crate::upload_source::SourceRevision::from_metadata(&self.destination.metadata()?)
        .with_receipts(&self.journal.metadata()?),
    )
  }

  pub(crate) fn upload_block(
    &mut self,
    index: usize,
    seed: &Seed,
  ) -> Result<(Vec<u8>, Proof), TransferError> {
    if self.failed {
      return Err(invalid("destination is unavailable for sharing"));
    }
    let offset = self
      .receipts
      .get(index)
      .copied()
      .flatten()
      .ok_or_else(|| invalid("block has no committed receipt"))?;
    let bytes = self.read_verified(index)?;
    // All journal access is under Destination's lock. Appends seek to EOF;
    // a failed proof read cannot redirect a later checkpoint write.
    self.journal.seek(SeekFrom::Start(offset))?;
    let line = read_record(&mut BufReader::new(&mut self.journal))?
      .ok_or_else(|| invalid("missing committed receipt"))?;
    let receipt: Receipt = serde_json::from_slice(&line).map_err(|e| invalid(e.to_string()))?;
    if receipt.index != index
      || !q2q_core::merkle::verify(
        &bytes,
        index,
        seed.info.leaf_count(),
        &receipt.proof,
        &seed.info.root,
      )
    {
      return Err(invalid("shared block or proof changed"));
    }
    Ok((bytes, receipt.proof))
  }

  pub(crate) fn sync(&self) -> std::io::Result<()> {
    self.destination.sync_all()
  }

  pub(crate) fn create(seed: &Seed, path: &Path) -> Result<Self, TransferError> {
    let journal_path = sidecar(path, ".q2q.resume");
    if journal_path.try_exists()? {
      return Err(
        std::io::Error::new(
          std::io::ErrorKind::AlreadyExists,
          "download checkpoint already exists",
        )
        .into(),
      );
    }
    let destination = OpenOptions::new()
      .read(true)
      .write(true)
      .create_new(true)
      .open(path)?;
    destination
      .try_lock()
      .map_err(|e| invalid(format!("destination is locked: {e}")))?;
    // A stale receipt file must not be silently attached to a new destination.
    let mut journal = OpenOptions::new()
      .read(true)
      .write(true)
      .create_new(true)
      .open(&journal_path)?;
    destination.set_len(seed.info.length)?;
    destination.sync_all()?;
    write_record(
      &mut journal,
      &Header {
        version: 1,
        info_hash: seed.info_hash(),
      },
    )?;
    journal.sync_all()?;
    sync_parent(path)?;
    Ok(Self {
      store: FileStore::open(path, seed.info.length, seed.info.block_size)?,
      destination,
      journal,
      failed: false,
      receipts: vec![None; seed.info.leaf_count()],
    })
  }

  pub(crate) fn resume(seed: &Seed, path: &Path) -> Result<Self, TransferError> {
    let destination = OpenOptions::new().read(true).write(true).open(path)?;
    destination
      .try_lock()
      .map_err(|e| invalid(format!("destination is locked: {e}")))?;
    let mut store = FileStore::open(path, seed.info.length, seed.info.block_size)?;
    let journal_path = sidecar(path, ".q2q.resume");
    let mut reader = BufReader::new(File::open(&journal_path)?);
    let header: Header = serde_json::from_slice(
      &read_record(&mut reader)?.ok_or_else(|| invalid("missing checkpoint header"))?,
    )
    .map_err(|e| invalid(e.to_string()))?;
    if header.version != 1 || header.info_hash != seed.info_hash() {
      return Err(invalid("checkpoint belongs to another seed or version"));
    }
    // A unique temporary file avoids following or truncating an existing path.
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce).map_err(|e| invalid(e.to_string()))?;
    let nonce: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
    let temporary_path = sidecar(path, &format!(".q2q.resume.{nonce}.tmp"));
    let file = OpenOptions::new()
      .read(true)
      .write(true)
      .create_new(true)
      .open(&temporary_path)?;
    let temporary = Temporary(temporary_path);
    let mut journal = file;
    write_record(&mut journal, &header)?;
    let mut seen = vec![false; store.block_count()];
    let mut receipts = vec![None; store.block_count()];
    while let Some(line) = read_record(&mut reader)? {
      let receipt: Receipt = serde_json::from_slice(&line).map_err(|e| invalid(e.to_string()))?;
      if receipt.index >= seen.len() || seen[receipt.index] {
        return Err(invalid("checkpoint block index is invalid or duplicated"));
      }
      seen[receipt.index] = true;
      match store.verify_existing(receipt.index, &receipt.proof, &seed.info.root) {
        Ok(_) => {
          let offset = journal.stream_position()?;
          write_record(&mut journal, &receipt)?;
          receipts[receipt.index] = Some(offset);
        }
        // Corrupt bytes or forged receipts are missing blocks, never verified.
        Err(StoreError::RootMismatch) => {}
        Err(error) => return Err(error.into()),
      }
    }
    drop(reader);
    // Compact validated receipts before appending. Rename commits the complete
    // replacement atomically; the old journal survives interruption beforehand.
    journal.sync_all()?;
    drop(journal);
    std::fs::rename(&temporary.0, &journal_path)?;
    sync_parent(path)?;
    let mut journal = OpenOptions::new()
      .read(true)
      .append(true)
      .open(journal_path)?;
    journal.seek(SeekFrom::End(0))?;
    Ok(Self {
      store,
      destination,
      journal,
      failed: false,
      receipts,
    })
  }

  pub(crate) fn commit(
    &mut self,
    index: usize,
    bytes: &[u8],
    proof: Proof,
    root: &Hash,
  ) -> Result<(), TransferError> {
    if self.failed {
      return Err(invalid(
        "destination stopped after a previous disk/checkpoint failure",
      ));
    }
    let result = self.commit_block(index, bytes, proof, root);
    self.failed = result.is_err();
    result
  }

  fn commit_block(
    &mut self,
    index: usize,
    bytes: &[u8],
    proof: Proof,
    root: &Hash,
  ) -> Result<(), TransferError> {
    for (piece, data) in bytes.chunks(PIECE_SIZE).enumerate() {
      self.store.write_piece(index, piece, data)?;
    }
    self
      .store
      .verify(index, Some(&proof), root)
      .map_err(|error| match error {
        StoreError::RootMismatch => invalid("written block does not match its verified proof"),
        other => TransferError::Store(other),
      })?;
    // Durable bytes precede their receipt. An interrupted write has no receipt
    // and will simply be requested again after restart.
    self.destination.sync_data()?;
    let offset = self.journal.seek(SeekFrom::End(0))?;
    write_record(&mut self.journal, &Receipt { index, proof })?;
    self.journal.sync_data()?;
    // Publish the index only after both data and receipt are durable.
    self.receipts[index] = Some(offset);
    Ok(())
  }
}

#[cfg(test)]
#[path = "../tests/download_store.rs"]
pub(crate) mod tests;
