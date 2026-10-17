//! Managed publication lifecycle.

pub use crate::source_file::{PrepareError, PreparedFile};
pub use crate::upload_progress::{
  PublicationState, UploadConnectionProgress, UploadConnectionState, UploadProgress,
};

use crate::control::ControlSession;
use crate::error::TransferError;
use q2q_core::merkle::Hash;
use q2q_core::seed::Seed;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Semaphore, oneshot, watch};
use tokio::task::JoinHandle;

/// A live source. Dropping the handle stops serving and asynchronously withdraws
/// the Track holding; `close` waits for withdrawal and reports any failure.
pub struct Publication {
  seed: Seed,
  progress: Arc<crate::upload_progress::Progress>,
  stop: watch::Sender<bool>,
  task: Option<JoinHandle<Result<(), TransferError>>>,
}

impl Publication {
  pub(crate) async fn start(
    source: Arc<dyn crate::upload_source::UploadSource>,
    session: Arc<ControlSession>,
    router: Arc<crate::upload_router::UploadRouter>,
    wait_ready: bool,
    permits: Arc<Semaphore>,
    rate: Option<Arc<crate::upload_rate::UploadRate>>,
    cache: Arc<crate::upload_cache::UploadCache>,
  ) -> Result<Self, TransferError> {
    // Claim before touching the shared Punch gate. A conflicting low-level
    // accept returns an error instead of being paused or stealing requests.
    let registration = router
      .register(session.clone(), source.seed().info_hash())
      .await?;
    let seed = source.seed().clone();
    let progress = crate::upload_progress::Progress::new();
    let (stop, stopped) = watch::channel(false);
    let (ready, started) = oneshot::channel();
    let runner = PublicationTask {
      hash: seed.info_hash(),
      source,
      uploads: crate::upload::UploadPool::new(permits, rate, progress.clone()).with_cache(&cache),
      session,
      status: progress.run_guard(),
      progress: progress.clone(),
      registration,
    };
    let publication = Self {
      seed,
      progress,
      stop,
      task: Some(tokio::spawn(runner.run(stopped, ready))),
    };
    if !wait_ready {
      return Ok(publication);
    }
    match started.await {
      Ok(()) => Ok(publication),
      Err(_) => {
        // The runner owns startup errors and the common cleanup path. Joining
        // it preserves the original error, including an announce failure.
        publication.close().await?;
        Err(TransferError::Protocol(
          "publication stopped before announcing".into(),
        ))
      }
    }
  }

  /// Current counters, including piece sends not yet emitted to subscribers.
  pub fn progress(&self) -> UploadProgress {
    self.progress.snapshot()
  }

  /// Coalesced snapshots (at most every 100 ms during normal operation).
  /// Closing and terminal states are sent immediately. Clone watch values
  /// before awaiting; do not hold a watch borrow across an await.
  pub fn subscribe_progress(&self) -> watch::Receiver<UploadProgress> {
    self.progress.subscribe()
  }

  pub fn seed(&self) -> &Seed {
    &self.seed
  }
  pub fn info_hash(&self) -> Hash {
    self.seed.info_hash()
  }
  pub fn is_finished(&self) -> bool {
    self.task.as_ref().is_none_or(|task| task.is_finished())
  }

  pub async fn close(mut self) -> Result<(), TransferError> {
    self.progress.closing();
    let _ = self.stop.send(true);
    let result = self.task.as_mut().expect("publication task exists").await?;
    self.task.take();
    result
  }
}

impl Drop for Publication {
  fn drop(&mut self) {
    self.progress.closing();
    let _ = self.stop.send(true);
  }
}

/// Owns every resource until withdrawal and blocking reads have finished.
/// The leases are declared in release order for cancellation/unwinding too.
struct PublicationTask {
  hash: Hash,
  source: Arc<dyn crate::upload_source::UploadSource>,
  uploads: crate::upload::UploadPool,
  session: Arc<ControlSession>,
  status: crate::upload_progress::RunGuard,
  progress: Arc<crate::upload_progress::Progress>,
  registration: crate::upload_router::Registration,
}
impl PublicationTask {
  async fn run(
    mut self,
    stopped: watch::Receiver<bool>,
    ready: oneshot::Sender<()>,
  ) -> Result<(), TransferError> {
    let session = self.session.clone();
    let result = tokio::select! {
      result = self.serve(stopped, ready) => result,
      error = session.keep_online() => Err(TransferError::Control(error)),
    };
    self.progress.closing();
    // All startup, stop and error paths use this order. In particular, keep
    // admission ownership until all workers/reads and withdrawal are done.
    self.registration.incoming.close();
    self.uploads.stop().await;
    let withdrawn = self
      .session
      .update_files(&[], &[self.hash])
      .await
      .map(|_| ())
      .map_err(TransferError::Control);
    let result = result.and(withdrawn);
    self.registration.close().await;
    // A terminal snapshot permits immediate republishing: both leases have
    // already been released and no worker can still mutate the old source.
    self.progress.finish(result.is_err());
    drop(self.status);
    result
  }

  async fn serve(
    &mut self,
    mut stopped: watch::Receiver<bool>,
    ready: oneshot::Sender<()>,
  ) -> Result<(), TransferError> {
    let mut available = self.source.availability();
    if available.borrow().failed {
      return Err(TransferError::Protocol(
        "shared destination unavailable".into(),
      ));
    }
    // Registration exists before the first announcement; an empty destination
    // remains invisible until a durable, verified block is ready to serve.
    while !available.borrow().any() && !available.borrow().complete {
      tokio::select! {
        _ = stopped.changed() => return Ok(()),
        changed = available.changed() => {
          if changed.is_err() || available.borrow().failed { return Err(TransferError::Protocol("shared destination unavailable".into())); }
        }
      }
    }
    let (expires, complete) = announce(&self.session, self.hash, &*self.source).await?;
    let _ = ready.send(());
    tokio::select! {
      result = self.uploads.run(&mut self.registration.incoming, self.source.clone()) => result,
      result = renew_holding(&self.session, self.hash, &*self.source, expires, complete) => result,
      _ = stopped.changed() => Ok(()),
      _ = self.progress.coalesce() => unreachable!("progress timer does not terminate"),
    }
  }
}

async fn renew_holding(
  session: &Arc<ControlSession>,
  hash: Hash,
  source: &dyn crate::upload_source::UploadSource,
  mut expires: u64,
  mut complete: bool,
) -> Result<(), TransferError> {
  let mut available = source.availability();
  let mut changing = available.has_changed().is_ok();
  loop {
    let delay = expires.saturating_sub(q2q_track::state::now()).max(2) / 2;
    let renewal = tokio::time::sleep(Duration::from_secs(delay));
    tokio::pin!(renewal);
    loop {
      // Subscribe before checking: completion during the previous announce
      // still upgrades immediately, and ongoing blocks cannot postpone TTL.
      let latest = available.borrow_and_update().clone();
      if latest.failed {
        return Err(TransferError::Protocol(
          "shared destination unavailable".into(),
        ));
      }
      if latest.complete && !complete {
        break;
      }
      tokio::select! {
        _ = &mut renewal => break,
        changed = available.changed(), if changing => { if changed.is_err() { changing = false; } },
      }
    }
    (expires, complete) = announce(session, hash, source).await?;
  }
}

async fn announce(
  session: &Arc<ControlSession>,
  hash: Hash,
  source: &dyn crate::upload_source::UploadSource,
) -> Result<(u64, bool), TransferError> {
  let available = source.availability().borrow().clone();
  if available.failed {
    return Err(TransferError::Protocol(
      "shared destination unavailable".into(),
    ));
  }
  let complete = available.complete;
  let full = if complete { vec![hash] } else { vec![] };
  Ok((
    session.update_holdings(&[hash], &[], &full).await?,
    complete,
  ))
}
