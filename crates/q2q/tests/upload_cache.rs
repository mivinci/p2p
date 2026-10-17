use super::*;
use crate::{
  error::TransferError,
  source_file::PreparedFile,
  task_scope::TaskScope,
  upload_session::UploadContext,
  upload_source::{Availability, SourceRevision, UploadSource},
};
use q2q_core::seed::Seed;
use std::{
  path::Path,
  sync::{
    Condvar,
    atomic::{AtomicBool, AtomicUsize, Ordering},
  },
  time::Duration,
};
use tokio::sync::Semaphore;

#[derive(Default)]
struct Gate {
  open: Mutex<bool>,
  changed: Condvar,
}
impl Gate {
  fn release(&self) {
    *self.open.lock().unwrap() = true;
    self.changed.notify_all();
  }
  fn wait(&self) {
    let mut open = self.open.lock().unwrap();
    while !*open {
      open = self.changed.wait(open).unwrap();
    }
  }
}
struct Counted {
  source: Arc<dyn UploadSource>,
  calls: AtomicUsize,
  started: watch::Sender<usize>,
  gate: Option<Arc<Gate>>,
  fail: AtomicBool,
}
impl Counted {
  fn new(source: Arc<dyn UploadSource>, gate: Option<Arc<Gate>>) -> Arc<Self> {
    Arc::new(Self {
      source,
      calls: AtomicUsize::new(0),
      started: watch::channel(0).0,
      gate,
      fail: AtomicBool::new(false),
    })
  }
  async fn entered(&self, count: usize) {
    let mut started = self.started.subscribe();
    tokio::time::timeout(Duration::from_secs(3), async {
      loop {
        if *started.borrow_and_update() >= count {
          return;
        }
        started.changed().await.unwrap();
      }
    })
    .await
    .unwrap();
  }
}
impl UploadSource for Counted {
  fn seed(&self) -> &Seed {
    self.source.seed()
  }
  fn availability(&self) -> watch::Receiver<Availability> {
    self.source.availability()
  }
  fn read_limit(&self) -> Arc<Semaphore> {
    self.source.read_limit()
  }
  fn revision(&self, index: usize) -> Result<SourceRevision, TransferError> {
    self.source.revision(index)
  }
  fn read_block(&self, index: usize) -> Result<(Vec<u8>, Proof), TransferError> {
    self.calls.fetch_add(1, Ordering::SeqCst);
    self.started.send_modify(|n| *n += 1);
    if let Some(gate) = &self.gate {
      gate.wait();
    }
    if self.fail.load(Ordering::SeqCst) {
      return Err(std::io::Error::other("injected read error").into());
    }
    self.source.read_block(index)
  }
}
fn prepared(directory: &Path, value: u8) -> PreparedFile {
  let path = directory.join(format!("source-{value}"));
  std::fs::write(&path, vec![value; 3 * 262144 + 13]).unwrap();
  PreparedFile::open(path, "http://localhost".into()).unwrap()
}
async fn read(
  cache: Arc<SourceCache>,
  source: Arc<dyn UploadSource>,
  index: usize,
) -> Result<Arc<VerifiedBlock>, TransferError> {
  UploadContext::unmanaged()
    .cached(Some(cache))
    .read_block(source, index)
    .await
}
fn managed(cache: Arc<SourceCache>) -> (UploadContext, TaskScope, Arc<Semaphore>) {
  let scope = TaskScope::new();
  let permits = Arc::new(Semaphore::new(1));
  let context = UploadContext::managed(
    scope.clone(),
    Arc::new(permits.clone().try_acquire_owned().unwrap()),
    None,
    None,
  )
  .cached(Some(cache));
  (context, scope, permits)
}

#[tokio::test]
async fn simultaneous_readers_share_one_verified_allocation_and_later_hits() {
  let dir = tempfile::tempdir().unwrap();
  let gate = Arc::new(Gate::default());
  let source = Counted::new(Arc::new(prepared(dir.path(), 7)), Some(gate.clone()));
  let pool = UploadCache::new(DEFAULT_CACHE_BYTES);
  let cache = pool.source();
  let mut tasks = tokio::task::JoinSet::new();
  for _ in 0..16 {
    tasks.spawn(read(cache.clone(), source.clone(), 0));
  }
  source.entered(1).await;
  tokio::task::yield_now().await;
  assert_eq!(source.calls.load(Ordering::SeqCst), 1);
  gate.release();
  let mut blocks = Vec::new();
  while let Some(result) = tasks.join_next().await {
    blocks.push(result.unwrap().unwrap());
  }
  assert!(blocks.iter().all(|block| Arc::ptr_eq(block, &blocks[0])));
  assert_eq!(&*blocks[0].bytes, &vec![7; 262144]);
  let again = read(cache, source.clone(), 0).await.unwrap();
  assert!(Arc::ptr_eq(&again, &blocks[0]));
  assert_eq!(source.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn distinct_blocks_keep_independent_bounded_reads() {
  let dir = tempfile::tempdir().unwrap();
  let gate = Arc::new(Gate::default());
  let prepared = prepared(dir.path(), 9).with_read_limit(Arc::new(Semaphore::new(2)));
  let source = Counted::new(Arc::new(prepared), Some(gate.clone()));
  let pool = UploadCache::new(DEFAULT_CACHE_BYTES);
  let cache = pool.source();
  let mut tasks = tokio::task::JoinSet::new();
  for index in 0..3 {
    tasks.spawn(read(cache.clone(), source.clone(), index));
  }
  source.entered(2).await;
  assert_eq!(source.calls.load(Ordering::SeqCst), 2);
  assert_eq!(source.read_limit().available_permits(), 0);
  gate.release();
  while let Some(result) = tasks.join_next().await {
    result.unwrap().unwrap();
  }
  assert_eq!(source.calls.load(Ordering::SeqCst), 3);
  assert_eq!(source.read_limit().available_permits(), 2);
}

#[tokio::test]
async fn lru_accounts_for_proof_bytes_and_keeps_active_evicted_data_valid() {
  let dir = tempfile::tempdir().unwrap();
  let prepared = prepared(dir.path(), 11);
  let item = VerifiedBlock::new(
    prepared.read_verified_block(0).unwrap(),
    prepared.proof(0).unwrap(),
    None,
  );
  assert!(item.cost() > item.bytes.len());
  let capacity = item.cost() * 2;
  let pool = UploadCache::new(capacity);
  let cache = pool.source();
  let source = Counted::new(Arc::new(prepared), None);
  let first = read(cache.clone(), source.clone(), 0).await.unwrap();
  let second = read(cache.clone(), source.clone(), 1).await.unwrap();
  let hot = read(cache.clone(), source.clone(), 0).await.unwrap();
  assert!(Arc::ptr_eq(&hot, &first));
  read(cache.clone(), source.clone(), 2).await.unwrap();
  assert!(pool.budget.lock().unwrap().used <= capacity);
  assert_eq!(&*second.bytes, &vec![11; 262144]);
  let next = read(cache, source.clone(), 1).await.unwrap();
  assert!(!Arc::ptr_eq(&next, &second));
  assert_eq!(source.calls.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn zero_budget_still_merges_inflight_reads_and_never_retains_entries() {
  let dir = tempfile::tempdir().unwrap();
  let gate = Arc::new(Gate::default());
  let source = Counted::new(Arc::new(prepared(dir.path(), 13)), Some(gate.clone()));
  let pool = UploadCache::new(0);
  let cache = pool.source();
  let owner = tokio::spawn(read(cache.clone(), source.clone(), 0));
  source.entered(1).await;
  let waiter = read(cache.clone(), source.clone(), 0);
  tokio::pin!(waiter);
  assert!(
    tokio::time::timeout(Duration::from_millis(10), &mut waiter)
      .await
      .is_err()
  );
  gate.release();
  let first = owner.await.unwrap().unwrap();
  let second = waiter.await.unwrap();
  assert!(Arc::ptr_eq(&first, &second));
  assert_eq!(source.calls.load(Ordering::SeqCst), 1);
  assert_eq!(pool.budget.lock().unwrap().used, 0);
  read(cache, source.clone(), 0).await.unwrap();
  assert_eq!(source.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn oversized_blocks_are_not_retained_and_namespaces_are_isolated() {
  let dir = tempfile::tempdir().unwrap();
  let a = Counted::new(Arc::new(prepared(dir.path(), 17)), None);
  let b = Counted::new(Arc::new(prepared(dir.path(), 19)), None);
  let small = UploadCache::new(262144);
  let cache = small.source();
  for _ in 0..2 {
    read(cache.clone(), a.clone(), 0).await.unwrap();
  }
  assert_eq!(a.calls.load(Ordering::SeqCst), 2); // data + proof exceeds budget
  assert!(small.budget.lock().unwrap().entries.is_empty());
  let pool = UploadCache::new(DEFAULT_CACHE_BYTES);
  let ca = pool.source();
  let cb = pool.source();
  let left = read(ca.clone(), a.clone(), 0).await.unwrap();
  let right = read(cb.clone(), b.clone(), 0).await.unwrap();
  assert_ne!(left.bytes, right.bytes);
  ca.close();
  assert!(matches!(read(ca, a, 0).await, Err(TransferError::Closed)));
  assert!(Arc::ptr_eq(&right, &read(cb, b.clone(), 0).await.unwrap()));
  assert_eq!(b.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancelling_before_read_permit_releases_flight_without_spawning() {
  let dir = tempfile::tempdir().unwrap();
  let source = Counted::new(
    Arc::new(prepared(dir.path(), 23).with_read_limit(Arc::new(Semaphore::new(1)))),
    None,
  );
  let held = source.read_limit().acquire_owned().await.unwrap();
  let pool = UploadCache::new(DEFAULT_CACHE_BYTES);
  let cache = pool.source();
  assert!(
    tokio::time::timeout(
      Duration::from_millis(10),
      read(cache.clone(), source.clone(), 0)
    )
    .await
    .is_err()
  );
  assert!(cache.flights.lock().unwrap().active.is_empty());
  assert_eq!(source.calls.load(Ordering::SeqCst), 0);
  drop(held);
  read(cache, source.clone(), 0).await.unwrap();
  assert_eq!(source.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancelled_owner_keeps_both_permits_until_shared_blocking_read_finishes() {
  let dir = tempfile::tempdir().unwrap();
  let gate = Arc::new(Gate::default());
  let source = Counted::new(
    Arc::new(prepared(dir.path(), 29).with_read_limit(Arc::new(Semaphore::new(1)))),
    Some(gate.clone()),
  );
  let pool = UploadCache::new(DEFAULT_CACHE_BYTES);
  let cache = pool.source();
  let (context, scope, permits) = managed(cache.clone());
  let owner_source = source.clone();
  let owner = tokio::spawn(async move { context.read_block(owner_source, 0).await });
  source.entered(1).await;
  let waiter = tokio::spawn(read(cache, source.clone(), 0));
  owner.abort();
  assert!(matches!(owner.await, Err(error) if error.is_cancelled()));
  assert_eq!(permits.available_permits(), 0);
  assert_eq!(source.read_limit().available_permits(), 0);
  assert!(
    tokio::time::timeout(Duration::from_millis(10), scope.idle())
      .await
      .is_err()
  );
  gate.release();
  waiter.await.unwrap().unwrap();
  scope.idle().await;
  assert_eq!(permits.available_permits(), 1);
  assert_eq!(source.read_limit().available_permits(), 1);
  assert_eq!(source.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancelling_waiter_does_not_abort_owner_or_poison_cache() {
  let dir = tempfile::tempdir().unwrap();
  let gate = Arc::new(Gate::default());
  let source = Counted::new(Arc::new(prepared(dir.path(), 31)), Some(gate.clone()));
  let pool = UploadCache::new(DEFAULT_CACHE_BYTES);
  let cache = pool.source();
  let owner = tokio::spawn(read(cache.clone(), source.clone(), 0));
  source.entered(1).await;
  assert!(
    tokio::time::timeout(
      Duration::from_millis(10),
      read(cache.clone(), source.clone(), 0)
    )
    .await
    .is_err()
  );
  assert!(!cache.is_closed());
  gate.release();
  let first = owner.await.unwrap().unwrap();
  assert!(Arc::ptr_eq(
    &first,
    &read(cache, source.clone(), 0).await.unwrap()
  ));
  assert_eq!(source.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn close_wakes_waiters_and_prevents_late_backfill() {
  let dir = tempfile::tempdir().unwrap();
  let gate = Arc::new(Gate::default());
  let source = Counted::new(Arc::new(prepared(dir.path(), 37)), Some(gate.clone()));
  let pool = UploadCache::new(DEFAULT_CACHE_BYTES);
  let cache = pool.source();
  let (context, scope, permits) = managed(cache.clone());
  let owner_source = source.clone();
  let owner = tokio::spawn(async move { context.read_block(owner_source, 0).await });
  source.entered(1).await;
  let waiter = read(cache.clone(), source.clone(), 0);
  tokio::pin!(waiter);
  assert!(
    tokio::time::timeout(Duration::from_millis(10), &mut waiter)
      .await
      .is_err()
  );
  cache.close();
  assert!(matches!(waiter.await, Err(TransferError::Closed)));
  assert_eq!(permits.available_permits(), 0);
  gate.release();
  assert!(matches!(owner.await.unwrap(), Err(TransferError::Closed)));
  scope.idle().await;
  assert_eq!(permits.available_permits(), 1);
  assert!(pool.budget.lock().unwrap().entries.is_empty());
  assert_eq!(pool.budget.lock().unwrap().used, 0);
}

#[tokio::test]
async fn failed_loads_never_populate_cache_and_new_publications_can_retry() {
  let dir = tempfile::tempdir().unwrap();
  let source = Counted::new(Arc::new(prepared(dir.path(), 41)), None);
  let pool = UploadCache::new(DEFAULT_CACHE_BYTES);
  let cache = pool.source();
  source.fail.store(true, Ordering::SeqCst);
  assert!(matches!(
    read(cache.clone(), source.clone(), 0).await,
    Err(TransferError::Io(_))
  ));
  assert!(cache.is_closed());
  assert!(pool.budget.lock().unwrap().entries.is_empty());
  source.fail.store(false, Ordering::SeqCst);
  read(pool.source(), source.clone(), 0).await.unwrap();
  assert_eq!(source.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn warm_cache_still_detects_full_source_metadata_changes() {
  let dir = tempfile::tempdir().unwrap();
  let prepared = prepared(dir.path(), 43);
  let path = prepared.path().to_path_buf();
  let source = Counted::new(Arc::new(prepared), None);
  let pool = UploadCache::new(DEFAULT_CACHE_BYTES);
  let cache = pool.source();
  read(cache.clone(), source.clone(), 0).await.unwrap();
  std::fs::OpenOptions::new()
    .write(true)
    .open(path)
    .unwrap()
    .set_len(1)
    .unwrap();
  assert!(matches!(
    read(cache, source, 0).await,
    Err(TransferError::Prepare(
      crate::source_file::PrepareError::SourceChanged
    ))
  ));
  assert_eq!(pool.budget.lock().unwrap().used, 0);
}

#[cfg(unix)]
#[tokio::test]
async fn restored_mtime_invalidates_warm_cache_using_ctime_and_rechecks_bytes() {
  use std::io::Write;
  let dir = tempfile::tempdir().unwrap();
  let prepared = prepared(dir.path(), 47);
  let path = prepared.path().to_path_buf();
  let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
  let source = Counted::new(Arc::new(prepared), None);
  let pool = UploadCache::new(DEFAULT_CACHE_BYTES);
  let cache = pool.source();
  read(cache.clone(), source.clone(), 0).await.unwrap();
  let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
  file.write_all(&[255]).unwrap();
  file.set_modified(modified).unwrap();
  assert_eq!(file.metadata().unwrap().modified().unwrap(), modified);
  assert!(matches!(
    read(cache, source.clone(), 0).await,
    Err(TransferError::Prepare(
      crate::source_file::PrepareError::SourceChanged
    ))
  ));
  assert_eq!(source.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn growing_destinations_revalidate_and_cancelled_sharing_cannot_hit_cache() {
  let dir = tempfile::tempdir().unwrap();
  let prepared = prepared(dir.path(), 53);
  let destination =
    crate::destination::Destination::open(prepared.seed(), &dir.path().join("download"), false)
      .await
      .unwrap();
  let commit = |index| {
    let destination = destination.clone();
    let prepared = prepared.clone();
    async move {
      let bits = destination.bitfield().await.unwrap();
      let queue = crate::block_queue::WorkQueue::new(
        prepared.seed(),
        &bits,
        crate::progress::channel(prepared.seed(), &bits),
      );
      let bits = [0x80u8 >> index];
      destination
        .commit(
          queue.claim_available(&bits).unwrap(),
          prepared.read_verified_block(index).unwrap(),
          prepared.proof(index).unwrap(),
        )
        .await
        .unwrap();
    }
  };
  commit(0).await;
  let source = Counted::new(destination.upload_source(Arc::new(Semaphore::new(2))), None);
  assert!(!source.availability().borrow().has(1));
  let pool = UploadCache::new(DEFAULT_CACHE_BYTES);
  let cache = pool.source();
  read(cache.clone(), source.clone(), 0).await.unwrap();
  commit(1).await;
  read(cache.clone(), source.clone(), 0).await.unwrap();
  assert_eq!(source.calls.load(Ordering::SeqCst), 2);
  destination.stop_sharing();
  assert!(matches!(
    read(cache, source, 0).await,
    Err(TransferError::Closed)
  ));
  assert_eq!(pool.budget.lock().unwrap().used, 0);
}

#[test]
fn stale_invalidation_never_removes_a_newer_entry_and_last_owner_cleans_up() {
  let pool = UploadCache::new(4096);
  let cache = pool.source();
  let a = VerifiedBlock::new(vec![1], Proof::default(), None);
  let b = VerifiedBlock::new(vec![2], Proof::default(), None);
  pool.insert((cache.id, 0), a.clone());
  pool.insert((cache.id, 0), b.clone());
  cache.invalidate(0, &a);
  assert!(Arc::ptr_eq(&pool.get((cache.id, 0)).unwrap(), &b));
  drop(cache);
  assert_eq!(pool.budget.lock().unwrap().used, 0);
}

#[tokio::test]
async fn every_publication_shares_one_client_budget() {
  let dir = tempfile::tempdir().unwrap();
  let prepared = prepared(dir.path(), 59);
  let cost = VerifiedBlock::new(
    prepared.read_verified_block(0).unwrap(),
    prepared.proof(0).unwrap(),
    None,
  )
  .cost();
  let a = Counted::new(Arc::new(prepared), None);
  let b = Counted::new(Arc::new(self::prepared(dir.path(), 61)), None);
  let pool = UploadCache::new(cost);
  let ca = pool.source();
  let cb = pool.source();
  read(ca.clone(), a.clone(), 0).await.unwrap();
  read(cb.clone(), b.clone(), 0).await.unwrap();
  assert_eq!(pool.budget.lock().unwrap().entries.len(), 1);
  assert!(pool.get((ca.id, 0)).is_none());
  assert!(pool.get((cb.id, 0)).is_some());
  assert!(pool.budget.lock().unwrap().used <= cost);
  read(ca, a.clone(), 0).await.unwrap();
  assert_eq!(a.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn proof_only_changes_and_failed_downloads_cannot_reuse_cached_receipts() {
  use std::io::Write;
  let dir = tempfile::tempdir().unwrap();
  let prepared = prepared(dir.path(), 67);
  let path = dir.path().join("download");
  let destination = crate::destination::Destination::open(prepared.seed(), &path, false)
    .await
    .unwrap();
  let bits = [0];
  let queue = crate::block_queue::WorkQueue::new(
    prepared.seed(),
    &bits,
    crate::progress::channel(prepared.seed(), &bits),
  );
  destination
    .commit(
      queue.claim_available(&[0x80]).unwrap(),
      prepared.read_verified_block(0).unwrap(),
      prepared.proof(0).unwrap(),
    )
    .await
    .unwrap();
  let source = Counted::new(destination.upload_source(Arc::new(Semaphore::new(1))), None);
  let pool = UploadCache::new(DEFAULT_CACHE_BYTES);
  let cache = pool.source();
  read(cache.clone(), source.clone(), 0).await.unwrap();
  let journal = path.with_file_name("download.q2q.resume");
  let text = std::fs::read_to_string(&journal).unwrap();
  let header_len = text.lines().next().unwrap().len();
  // Keep the file length unchanged so size alone cannot detect corruption.
  let mut corrupted = text.into_bytes();
  let receipt = header_len + 1;
  corrupted[receipt] = b'x';
  std::fs::OpenOptions::new()
    .write(true)
    .open(journal)
    .unwrap()
    .write_all(&corrupted)
    .unwrap();
  assert!(read(cache, source.clone(), 0).await.is_err());
  assert_eq!(source.calls.load(Ordering::SeqCst), 2);
  assert!(source.availability().borrow().failed);
  assert_eq!(pool.budget.lock().unwrap().used, 0);
}
