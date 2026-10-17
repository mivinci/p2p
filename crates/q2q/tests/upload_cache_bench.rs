//! Deterministic warm-filesystem microbenchmark; no throughput assertions.
use crate::{
  destination::Destination,
  error::TransferError,
  source_file::PreparedFile,
  upload_session::UploadContext,
  upload_source::{Availability, SourceRevision, UploadSource},
};
use q2q_core::{merkle::Proof, seed::Seed};
use std::{
  sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
  },
  time::Instant,
};
use tokio::sync::{Semaphore, watch};
struct Counted {
  source: Arc<dyn UploadSource>,
  reads: AtomicUsize,
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
    self.reads.fetch_add(1, Ordering::Relaxed);
    self.source.read_block(index)
  }
}
async fn commit(destination: &Destination, prepared: &PreparedFile, index: usize) {
  let bits = destination.bitfield().await.unwrap();
  let queue = crate::block_queue::WorkQueue::new(
    prepared.seed(),
    &bits,
    crate::progress::channel(prepared.seed(), &bits),
  );
  let mut available = vec![0; prepared.block_count().div_ceil(8)];
  available[index / 8] |= 1 << (7 - index % 8);
  destination
    .commit(
      queue.claim_available(&available).unwrap(),
      prepared.read_verified_block(index).unwrap(),
      prepared.proof(index).unwrap(),
    )
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "manual warm-filesystem source-read benchmark; run with --ignored --nocapture"]
async fn source_read_workloads() {
  let directory = tempfile::tempdir().unwrap();
  let path = directory.path().join("source");
  let bytes: Vec<_> = (0..128 * 262144 + 13)
    .map(|index| (index % 251) as u8)
    .collect();
  std::fs::write(&path, bytes).unwrap();
  let prepared = PreparedFile::open(path, "http://localhost".into()).unwrap();
  for cached in [false, true] {
    for workload in ["same", "distinct", "seek", "growing"] {
      let destination = if workload == "growing" {
        let destination = Destination::open(
          prepared.seed(),
          &directory.path().join(format!("destination-{cached}")),
          false,
        )
        .await
        .unwrap();
        commit(&destination, &prepared, 0).await;
        Some(destination)
      } else {
        None
      };
      let source: Arc<dyn UploadSource> = match &destination {
        Some(destination) => destination.upload_source(Arc::new(Semaphore::new(4))),
        None => Arc::new(prepared.clone()),
      };
      let counted = Arc::new(Counted {
        source,
        reads: AtomicUsize::new(0),
      });
      let cache = crate::upload_cache::UploadCache::new(crate::upload_cache::DEFAULT_CACHE_BYTES);
      let namespace = if cached { Some(cache.source()) } else { None };
      let start = Instant::now();
      for round in 0..16 {
        let mut tasks = tokio::task::JoinSet::new();
        for worker in 0..8 {
          let source = counted.clone();
          let index = match workload {
            "distinct" => worker * 16 + round,
            "seek" => {
              if round == 15 {
                128
              } else {
                (worker * 7 + round * 3) % 16
              }
            }
            _ => 0,
          };
          let namespace = namespace.clone();
          tasks.spawn(async move {
            UploadContext::unmanaged()
              .cached(namespace)
              .read_block(source, index)
              .await
              .unwrap();
          });
        }
        while let Some(result) = tasks.join_next().await {
          result.unwrap();
        }
        if let Some(destination) = &destination {
          commit(destination, &prepared, round + 1).await;
        }
      }
      eprintln!(
        "cache={cached} workload={workload} requests=128 verified_reads={} elapsed_ms={}",
        counted.reads.load(Ordering::Relaxed),
        start.elapsed().as_millis()
      );
      if !cached {
        assert_eq!(counted.reads.load(Ordering::Relaxed), 128);
      }
      if cached && workload == "same" {
        assert_eq!(counted.reads.load(Ordering::Relaxed), 1);
      }
      if cached && workload == "distinct" {
        assert_eq!(counted.reads.load(Ordering::Relaxed), 128);
      }
    }
  }
}
