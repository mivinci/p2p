//! Shared destination ownership and the only asynchronous disk access boundary.

use crate::block_queue::BlockLease;
use crate::download_store::DownloadStore;
use crate::error::TransferError;
use q2q_core::file::StoreError;
use q2q_core::merkle::{self, Proof};
use q2q_core::seed::Seed;
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub(crate) struct Destination {
  store: Arc<Mutex<DownloadStore>>,
  seed: Arc<Seed>,
  available: tokio::sync::watch::Sender<crate::upload_source::Availability>,
}
impl Destination {
  pub(crate) async fn open(seed: &Seed, path: &Path, resume: bool) -> Result<Self, TransferError> {
    check_seed(seed)?;
    let seed = Arc::new(seed.clone());
    let content = seed.clone();
    let path = path.to_path_buf();
    let store = tokio::task::spawn_blocking(move || {
      if resume {
        DownloadStore::resume(&content, &path)
      } else {
        DownloadStore::create(&content, &path)
      }
    })
    .await??;
    let available = tokio::sync::watch::channel(crate::upload_source::Availability {
      bits: store.verified_bitfield(),
      failed: false,
      complete: false,
    })
    .0;
    Ok(Self {
      available,
      store: Arc::new(Mutex::new(store)),
      seed,
    })
  }

  async fn access<T: Send + 'static>(
    &self,
    operation: impl FnOnce(&mut DownloadStore) -> Result<T, TransferError> + Send + 'static,
  ) -> Result<T, TransferError> {
    let store = self.store.clone();
    tokio::task::spawn_blocking(move || {
      let mut store = store
        .lock()
        .map_err(|_| TransferError::Checkpoint("destination lock poisoned".into()))?;
      operation(&mut store)
    })
    .await?
  }

  pub(crate) async fn bitfield(&self) -> Result<Vec<u8>, TransferError> {
    self.access(|store| Ok(store.verified_bitfield())).await
  }
  pub(crate) async fn complete(&self) -> Result<bool, TransferError> {
    self.access(|store| store.complete()).await
  }

  pub(crate) async fn commit(
    &self,
    lease: BlockLease,
    bytes: Vec<u8>,
    proof: Proof,
  ) -> Result<(), TransferError> {
    let seed = self.seed.clone();
    let store = self.store.clone();
    let guard = lease.disk_task();
    let available = self.available.clone();
    tokio::task::spawn_blocking(move || {
      let _guard = guard;
      let index = lease.index;
      let length =
        crate::progress::block_len(seed.info.length, seed.info.block_size, index) as usize;
      if bytes.len() != length
        || !merkle::verify(
          &bytes,
          index,
          seed.info.leaf_count(),
          &proof,
          &seed.info.root,
        )
      {
        return Err(TransferError::Store(StoreError::RootMismatch));
      }
      let mut store = store
        .lock()
        .map_err(|_| TransferError::Checkpoint("destination lock poisoned".into()))?;
      if let Err(error) = store.commit(index, &bytes, proof, &seed.info.root) {
        available.send_modify(|state| state.failed = true);
        return Err(error);
      }
      available.send_modify(|state| state.bits[index / 8] |= 1 << (7 - index % 8));
      lease.commit();
      Ok(())
    })
    .await?
  }
  pub(crate) async fn read_verified(&self, index: usize) -> Result<Vec<u8>, TransferError> {
    self.access(move |store| store.read_verified(index)).await
  }
  pub(crate) async fn finish(&self) -> Result<(), TransferError> {
    self
      .access(|store| store.sync().map_err(Into::into))
      .await?;
    if self.complete().await? {
      self.available.send_modify(|state| state.complete = true);
    }
    Ok(())
  }

  pub(crate) fn stop_sharing(&self) {
    self.available.send_modify(|state| state.failed = true);
  }

  pub(crate) fn upload_source(
    &self,
    reads: Arc<tokio::sync::Semaphore>,
  ) -> Arc<dyn crate::upload_source::UploadSource> {
    Arc::new(DownloadSource {
      destination: self.clone(),
      reads,
    })
  }

  // Fault-injection fixtures deliberately bypass the production access boundary.
  #[cfg(test)]
  pub(crate) fn lock(&self) -> std::sync::LockResult<std::sync::MutexGuard<'_, DownloadStore>> {
    self.store.lock()
  }
}

fn invalid_seed(message: impl Into<String>) -> TransferError {
  TransferError::Protocol(message.into())
}
pub(crate) fn check_seed(seed: &Seed) -> Result<(), TransferError> {
  seed.validate().map_err(|e| invalid_seed(e.to_string()))?;
  if seed.info.length.div_ceil(u64::from(seed.info.block_size))
    > q2q_core::peer_wire::MAX_BLOCKS as u64
  {
    return Err(invalid_seed("single-peer bitfield limit exceeded"));
  }
  if seed.info.length == 0 && seed.info.root != merkle::EMPTY_ROOT {
    return Err(invalid_seed("empty file has a nonempty Merkle root"));
  }
  Ok(())
}

pub(crate) struct DownloadGuard {
  store: Destination,
  finished: bool,
}
impl DownloadGuard {
  pub(crate) fn new(store: Destination) -> Self {
    Self {
      store,
      finished: false,
    }
  }
  pub(crate) fn finish(&mut self) {
    self.finished = true;
  }
}
impl Drop for DownloadGuard {
  fn drop(&mut self) {
    if !self.finished {
      self.store.stop_sharing();
    }
  }
}

struct DownloadSource {
  destination: Destination,
  reads: Arc<tokio::sync::Semaphore>,
}
impl crate::upload_source::UploadSource for DownloadSource {
  fn seed(&self) -> &Seed {
    &self.destination.seed
  }
  fn availability(&self) -> tokio::sync::watch::Receiver<crate::upload_source::Availability> {
    self.destination.available.subscribe()
  }
  fn read_limit(&self) -> Arc<tokio::sync::Semaphore> {
    self.reads.clone()
  }
  fn revision(&self, index: usize) -> Result<crate::upload_source::SourceRevision, TransferError> {
    if !self.destination.available.borrow().has(index) {
      return Err(TransferError::Closed);
    }
    self
      .destination
      .store
      .lock()
      .map_err(|_| TransferError::Checkpoint("destination lock poisoned".into()))?
      .upload_revision(index)
  }
  fn read_block(&self, index: usize) -> Result<(Vec<u8>, Proof), TransferError> {
    let result = self
      .destination
      .store
      .lock()
      .map_err(|_| TransferError::Checkpoint("destination lock poisoned".into()))?
      .upload_block(index, &self.destination.seed);
    if result.is_err() {
      self
        .destination
        .available
        .send_modify(|state| state.failed = true);
    }
    result
  }
}
