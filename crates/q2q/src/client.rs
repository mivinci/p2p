//! Client construction and identity lifecycle.

pub use crate::reader::{OpenOptions, VerifiedFile};

pub use crate::control::{ControlError, ControlSession, QueryOptions};
pub use crate::peer::PeerSession;
pub use crate::publication::{
  Publication, PublicationState, UploadConnectionProgress, UploadConnectionState, UploadProgress,
};
pub use crate::transfer::{Transfer, TransferError, TransferProgress, TransferState};

use crate::identity::Identity;
use crate::source_file::{PrepareError, PreparedFile};
use q2q_core::identity::PeerId;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{OnceCell, Semaphore};
use url::Url;

/// Start configuring a client. The builder performs no network requests.
pub fn builder() -> Builder {
  Builder::default()
}

#[derive(Default)]
pub struct Builder {
  track_url: Option<String>,
  identity_file: Option<PathBuf>,
  storage_dir: Option<PathBuf>,
  max_download_peers: Option<usize>,
  peer_query_limit: Option<usize>,
  max_upload_peers: Option<usize>,
  max_upload_reads: Option<usize>,
  upload_cache_bytes: Option<usize>,
  upload_rate_limit: Option<u64>,
}

impl Builder {
  /// Track's logical HTTP(S) address. Punch and TURN endpoints arrive later
  /// from Track's announce response.
  pub fn track_url(mut self, url: impl Into<String>) -> Self {
    self.track_url = Some(url.into());
    self
  }

  /// A versioned file containing the local Ed25519 private seed. A missing
  /// file is created with owner-only permissions on Unix; an invalid existing
  /// file is rejected instead of silently changing this peer's identity.
  pub fn identity_file(mut self, path: impl Into<PathBuf>) -> Self {
    self.identity_file = Some(path.into());
    self
  }

  /// Directory reserved for verified blocks and download progress.
  pub fn storage_dir(mut self, path: impl Into<PathBuf>) -> Self {
    self.storage_dir = Some(path.into());
    self
  }

  /// Maximum concurrent peer connections or CDN block requests, 1–16 (default 4).
  pub fn max_download_peers(mut self, count: usize) -> Self {
    self.max_download_peers = Some(count);
    self
  }

  /// Candidates requested per discovery query, 1–50. Defaults to twice
  /// max_download_peers (8 with default concurrency), independently of the
  /// connection budget. Track may return fewer candidates.
  pub fn peer_query_limit(mut self, count: usize) -> Self {
    self.peer_query_limit = Some(count);
    self
  }

  /// Maximum managed upload connections, including handshakes and idle VOD
  /// sessions, 1–16 (default 4). Independent of download concurrency.
  pub fn max_upload_peers(mut self, count: usize) -> Self {
    self.max_upload_peers = Some(count);
    self
  }

  /// Maximum in-flight source reads, metadata and hash checks, 1–16 (default 4).
  /// Shared by prepared sources and download sharing on this client. Independent
  /// of upload connections; waiting occurs before spawn_blocking.
  pub fn max_upload_reads(mut self, count: usize) -> Self {
    self.max_upload_reads = Some(count);
    self
  }

  /// Retained upload data/proof cache budget across this Client (default
  /// 16 MiB, maximum 256 MiB). Zero disables retention but still merges
  /// simultaneous reads of the same block. In-flight/send buffers are separate.
  pub fn upload_cache_bytes(mut self, bytes: usize) -> Self {
    self.upload_cache_bytes = Some(bytes);
    self
  }

  /// Total encoded piece bytes per second across managed uploads. Unset means
  /// unlimited; zero is invalid. Includes retransmits and the piece header,
  /// excludes control messages and WebRTC/transport overhead.
  pub fn upload_rate_limit(mut self, bytes_per_second: u64) -> Self {
    self.upload_rate_limit = Some(bytes_per_second);
    self
  }

  /// Validate the configuration and load or create the persistent identity.
  /// No connection to Track or Punch is opened yet.
  pub async fn build(self) -> Result<Client, BuildError> {
    if self.upload_rate_limit == Some(0) {
      return Err(BuildError::InvalidUploadRate);
    }
    let upload_cache_bytes = self
      .upload_cache_bytes
      .unwrap_or(crate::upload_cache::DEFAULT_CACHE_BYTES);
    if upload_cache_bytes > crate::upload_cache::MAX_CACHE_BYTES {
      return Err(BuildError::InvalidUploadCacheBytes);
    }
    let max_upload_reads = self.max_upload_reads.unwrap_or(4);
    if !(1..=16).contains(&max_upload_reads) {
      return Err(BuildError::InvalidUploadReads);
    }
    let max_upload_peers = self.max_upload_peers.unwrap_or(4);
    if !(1..=16).contains(&max_upload_peers) {
      return Err(BuildError::InvalidUploadPeers);
    }
    let max_download_peers = self.max_download_peers.unwrap_or(4);
    if !(1..=16).contains(&max_download_peers) {
      return Err(BuildError::InvalidDownloadPeers);
    }
    let peer_query_limit = self.peer_query_limit.unwrap_or(max_download_peers * 2);
    if !(1..=q2q_track::wire::MAX_QUERY_LIMIT).contains(&peer_query_limit) {
      return Err(BuildError::InvalidPeerQueryLimit);
    }
    let track_url = self.track_url.ok_or(BuildError::Missing("track_url"))?;
    let track_url = Url::parse(&track_url).map_err(BuildError::InvalidTrackUrl)?;
    if !matches!(track_url.scheme(), "http" | "https") || track_url.host_str().is_none() {
      return Err(BuildError::UnsupportedTrackUrl);
    }
    let identity_file = self
      .identity_file
      .ok_or(BuildError::Missing("identity_file"))?;
    let storage_dir = self.storage_dir.ok_or(BuildError::Missing("storage_dir"))?;

    tokio::fs::create_dir_all(&storage_dir)
      .await
      .map_err(BuildError::Storage)?;
    let identity = tokio::task::spawn_blocking(move || Identity::load_or_create(&identity_file))
      .await
      .map_err(BuildError::IdentityTask)?
      .map_err(BuildError::Identity)?;

    Ok(Client {
      track_url,
      storage_dir,
      identity,
      session: Arc::new(OnceCell::new()),
      upload_router: Arc::new(crate::upload_router::UploadRouter::default()),
      max_download_peers,
      peer_query_limit,
      uploads: Arc::new(Semaphore::new(max_upload_peers)),
      upload_reads: Arc::new(Semaphore::new(max_upload_reads)),
      upload_cache: crate::upload_cache::UploadCache::new(upload_cache_bytes),
      upload_rate: self
        .upload_rate_limit
        .map(|rate| Arc::new(crate::upload_rate::UploadRate::new(rate))),
    })
  }
}

/// A local peer that can publish and download through a shared control session.
pub struct Client {
  track_url: Url,
  storage_dir: PathBuf,
  identity: Identity,
  session: Arc<OnceCell<Arc<ControlSession>>>,
  upload_router: Arc<crate::upload_router::UploadRouter>,
  max_download_peers: usize,
  peer_query_limit: usize,
  uploads: Arc<Semaphore>,
  upload_reads: Arc<Semaphore>,
  upload_cache: Arc<crate::upload_cache::UploadCache>,
  upload_rate: Option<Arc<crate::upload_rate::UploadRate>>,
}

impl Client {
  /// Reuse or establish the authenticated Track session and Punch Binding.
  /// Publication/download handles maintain heartbeats while active; callers
  /// using this lower-level session alone must drive its heartbeat themselves.
  pub async fn connect(&self) -> Result<Arc<ControlSession>, ControlError> {
    self.connect_owned().await
  }

  pub(crate) fn connect_owned(
    &self,
  ) -> impl std::future::Future<Output = Result<Arc<ControlSession>, ControlError>> + Send + 'static
  {
    let cell = self.session.clone();
    let identity = self.identity.clone();
    let url = self.track_url.clone();
    async move {
      cell
        .get_or_try_init(|| async { ControlSession::connect(&identity, &url).await.map(Arc::new) })
        .await
        .cloned()
    }
  }

  /// Open a verified, seekable file with a bounded read-ahead window.
  pub fn open(
    &self,
    seed: &q2q_core::seed::Seed,
    path: impl Into<PathBuf>,
  ) -> crate::reader::OpenOptions<'_> {
    crate::reader::OpenOptions::new(self, seed.clone(), path.into(), self.max_download_peers)
  }

  /// Publish one local file and maintain its announcement until the handle is
  /// closed or dropped. Multiple content hashes share one upload router and the
  /// client's upload budgets; duplicate active hashes are rejected.
  pub async fn publish_file(&self, path: impl Into<PathBuf>) -> Result<Publication, TransferError> {
    let prepared = self.prepare_file(path).await?;
    crate::destination::check_seed(prepared.seed())?;
    let session = self.connect().await?;
    Publication::start(
      Arc::new(prepared),
      session,
      self.upload_router.clone(),
      true,
      self.uploads.clone(),
      self.upload_rate.clone(),
      self.upload_cache.clone(),
    )
    .await
  }

  /// Download from available peers concurrently, reassigning failed blocks.
  /// `wait` succeeds only after every block has
  /// passed verification; existing destination files are never overwritten.
  pub async fn download(
    &self,
    seed: &q2q_core::seed::Seed,
    path: impl Into<PathBuf>,
  ) -> Result<Transfer, TransferError> {
    self.start_download(seed, path.into(), false).await
  }

  /// Download while sharing durably verified blocks. The returned publication
  /// is independently owned: keep it after `Transfer::wait` to continue seeding,
  /// or close it to stop uploading without cancelling the download.
  /// Cancelling or failing the download withdraws its shared content.
  pub async fn download_shared(
    &self,
    seed: &q2q_core::seed::Seed,
    path: impl Into<PathBuf>,
  ) -> Result<(Transfer, Publication), TransferError> {
    self.start_shared_download(seed, path.into(), false).await
  }

  /// Resume and share only blocks revalidated from root-bound receipts.
  pub async fn resume_download_shared(
    &self,
    seed: &q2q_core::seed::Seed,
    path: impl Into<PathBuf>,
  ) -> Result<(Transfer, Publication), TransferError> {
    self.start_shared_download(seed, path.into(), true).await
  }

  async fn start_shared_download(
    &self,
    seed: &q2q_core::seed::Seed,
    path: PathBuf,
    resume: bool,
  ) -> Result<(Transfer, Publication), TransferError> {
    let store = crate::destination::Destination::open(seed, &path, resume).await?;
    let sharing = self.share_destination(&store).await?;
    let transfer = crate::download::start_with_store(
      self,
      seed,
      path,
      resume,
      self.max_download_peers,
      Some(store),
    )
    .await?;
    Ok((transfer, sharing))
  }

  pub(crate) async fn share_destination(
    &self,
    store: &crate::destination::Destination,
  ) -> Result<Publication, TransferError> {
    Publication::start(
      store.upload_source(self.upload_reads.clone()),
      self.connect().await?,
      self.upload_router.clone(),
      false,
      self.uploads.clone(),
      self.upload_rate.clone(),
      self.upload_cache.clone(),
    )
    .await
  }

  /// Resume an existing SDK destination using its `.q2q.resume` receipts.
  /// Every recorded block is read and reverified against the seed root; missing
  /// or corrupt blocks are fetched again. A complete file needs no network.
  pub async fn resume_download(
    &self,
    seed: &q2q_core::seed::Seed,
    path: impl Into<PathBuf>,
  ) -> Result<Transfer, TransferError> {
    self.start_download(seed, path.into(), true).await
  }

  async fn start_download(
    &self,
    seed: &q2q_core::seed::Seed,
    path: PathBuf,
    resume: bool,
  ) -> Result<Transfer, TransferError> {
    crate::download::start(self, seed, path, resume, self.max_download_peers).await
  }

  /// Prepare a local file for seeding without announcing it to the network.
  /// The returned handle checks each block again before it can be uploaded.
  pub async fn prepare_file(&self, path: impl Into<PathBuf>) -> Result<PreparedFile, PrepareError> {
    let path = path.into();
    let track_url = self.track_url.to_string();
    let reads = self.upload_reads.clone();
    tokio::task::spawn_blocking(move || {
      PreparedFile::open(path, track_url).map(|source| source.with_read_limit(reads))
    })
    .await
    .map_err(PrepareError::Task)?
  }

  pub(crate) fn peer_query_limit(&self) -> usize {
    self.peer_query_limit
  }

  pub fn peer_id(&self) -> &PeerId {
    self.identity.peer_id()
  }

  pub fn public_key(&self) -> [u8; 32] {
    self.identity.public_key()
  }

  pub fn track_url(&self) -> &Url {
    &self.track_url
  }

  pub fn storage_dir(&self) -> &Path {
    &self.storage_dir
  }
}

#[derive(Debug)]
pub enum BuildError {
  Missing(&'static str),
  InvalidTrackUrl(url::ParseError),
  UnsupportedTrackUrl,
  InvalidDownloadPeers,
  InvalidPeerQueryLimit,
  InvalidUploadPeers,
  InvalidUploadReads,
  InvalidUploadCacheBytes,
  InvalidUploadRate,
  Storage(io::Error),
  Identity(crate::identity::IdentityError),
  IdentityTask(tokio::task::JoinError),
}

impl fmt::Display for BuildError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Missing(field) => write!(f, "missing client setting: {field}"),
      Self::InvalidTrackUrl(e) => write!(f, "invalid Track URL: {e}"),
      Self::InvalidUploadRate => f.write_str("upload_rate_limit must be greater than zero"),
      Self::InvalidUploadCacheBytes => f.write_str("upload_cache_bytes must be at most 256 MiB"),
      Self::InvalidUploadReads => f.write_str("max_upload_reads must be between 1 and 16"),
      Self::InvalidUploadPeers => f.write_str("max_upload_peers must be between 1 and 16"),
      Self::InvalidPeerQueryLimit => f.write_str("peer_query_limit must be between 1 and 50"),
      Self::InvalidDownloadPeers => f.write_str("max_download_peers must be between 1 and 16"),
      Self::UnsupportedTrackUrl => f.write_str("Track URL must be HTTP or HTTPS with a host"),
      Self::Storage(e) => write!(f, "could not prepare client storage: {e}"),
      Self::Identity(e) => write!(f, "could not load client identity: {e}"),
      Self::IdentityTask(e) => write!(f, "identity task failed: {e}"),
    }
  }
}

impl std::error::Error for BuildError {
  fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
    match self {
      Self::InvalidTrackUrl(e) => Some(e),
      Self::Storage(e) => Some(e),
      Self::Identity(e) => Some(e),
      Self::IdentityTask(e) => Some(e),
      _ => None,
    }
  }
}

#[cfg(test)]
#[path = "../tests/client.rs"]
mod tests;
