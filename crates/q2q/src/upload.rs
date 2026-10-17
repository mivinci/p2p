//! Bounded upload admission, independent sessions and complete shutdown.
use crate::error::TransferError;
use crate::task_scope::TaskScope;
use crate::upload_progress::{Outcome, Progress, UploadConnectionState};
use crate::upload_session::UploadContext;
use crate::upload_source::UploadSource;
use q2q_punch::message::RejectReason;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

pub(crate) struct UploadPool {
  workers: JoinSet<Result<(), TransferError>>,
  permits: Arc<Semaphore>,
  scope: TaskScope,
  cache: Option<Arc<crate::upload_cache::SourceCache>>,
  rate: Option<Arc<crate::upload_rate::UploadRate>>,
  progress: Arc<Progress>,
}
impl UploadPool {
  pub(crate) fn new(
    permits: Arc<Semaphore>,
    rate: Option<Arc<crate::upload_rate::UploadRate>>,
    progress: Arc<Progress>,
  ) -> Self {
    Self {
      workers: JoinSet::new(),
      permits,
      rate,
      progress,
      scope: TaskScope::new(),
      cache: None,
    }
  }
  pub(crate) fn with_cache(mut self, cache: &Arc<crate::upload_cache::UploadCache>) -> Self {
    self.cache = Some(cache.source());
    self
  }

  pub(crate) async fn run(
    &mut self,
    incoming: &mut tokio::sync::mpsc::Receiver<crate::peer::PendingPeer>,
    source: Arc<dyn UploadSource>,
  ) -> Result<(), TransferError> {
    loop {
      tokio::select! {
        biased;
        result = self.workers.join_next(), if !self.workers.is_empty() => {
          match result.expect("worker exists")? {
            Ok(()) | Err(TransferError::Closed | TransferError::Timeout | TransferError::Protocol(_) | TransferError::Wire(_) | TransferError::Rejected(_)) => {},
            Err(error) => return Err(error),
          }
        }
        request = incoming.recv() => {
          let request = request.ok_or_else(|| TransferError::Protocol("upload router stopped".into()))?;
          // Reserve before Accept, so handshake and idle connections count too.
          // The session retains this permit through any detached blocking read.
          let permit = match self.permits.clone().try_acquire_owned() {
            Ok(permit) => Arc::new(permit),
            Err(_) => { let _ = tokio::time::timeout(Duration::from_secs(2), request.reject(RejectReason::Busy)).await; continue; }
          };
          let progress = self.progress.connection(request.connection_id().to_owned(), request.peer_id().clone());
          let context = UploadContext::managed(self.scope.clone(), permit, self.rate.clone(), Some(progress.clone())).cached(self.cache.clone());
          let source = source.clone();
          let task = self.scope.enter();
          self.workers.spawn(async move {
            let _task = task;
            let peer = match tokio::time::timeout(Duration::from_secs(45), request.accept()).await {
              Ok(Ok(peer)) => peer,
              // A failed handshake belongs to that requester, not the source.
              _ => { progress.finish(Outcome::Failed); return Ok(()); },
            };
            progress.state(UploadConnectionState::Idle);
            let result = peer.serve_file_scoped(source, context).await;
            progress.finish(if result.is_ok() { Outcome::Completed } else { Outcome::Failed });
            result
          });
        }

      }
    }
  }
  pub(crate) async fn stop(&mut self) {
    if let Some(cache) = &self.cache {
      cache.close();
    }
    self.workers.abort_all();
    while self.workers.join_next().await.is_some() {}
    self.scope.idle().await;
  }
}

impl Drop for UploadPool {
  fn drop(&mut self) {
    if let Some(cache) = &self.cache {
      cache.close();
    }
  }
}

#[cfg(test)]
#[path = "../tests/upload.rs"]
mod tests;
