use super::*;

#[tokio::test]
async fn download_concurrency_is_bounded_and_defaults_to_four() {
  let dir = tempfile::tempdir().unwrap();
  for count in [0, 17, usize::MAX] {
    assert!(matches!(
      builder().max_download_peers(count).build().await,
      Err(BuildError::InvalidDownloadPeers)
    ));
  }
  let make = || {
    builder()
      .track_url("http://127.0.0.1:1")
      .identity_file(dir.path().join("identity"))
      .storage_dir(dir.path().join("storage"))
  };
  assert_eq!(make().build().await.unwrap().max_download_peers, 4);
  assert_eq!(
    make()
      .max_download_peers(1)
      .build()
      .await
      .unwrap()
      .max_download_peers,
    1
  );
  assert_eq!(
    make()
      .max_download_peers(16)
      .build()
      .await
      .unwrap()
      .max_download_peers,
    16
  );
}

#[tokio::test]
async fn identity_survives_rebuilding_the_client() {
  let dir = tempfile::tempdir().unwrap();
  let identity_file = dir.path().join("identity");
  let storage_dir = dir.path().join("storage");
  let make_client = || {
    builder()
      .track_url("https://track.example.net")
      .identity_file(&identity_file)
      .storage_dir(&storage_dir)
  };

  let first = make_client().build().await.unwrap();
  let peer_id = first.peer_id().clone();
  let public_key = first.public_key();
  drop(first);

  let second = make_client().build().await.unwrap();
  assert_eq!(second.peer_id(), &peer_id);
  assert_eq!(second.public_key(), public_key);
}

#[tokio::test]
async fn corrupt_identity_is_rejected_without_replacement() {
  let dir = tempfile::tempdir().unwrap();
  let identity_file = dir.path().join("identity");
  std::fs::write(&identity_file, b"broken").unwrap();
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&identity_file, std::fs::Permissions::from_mode(0o600)).unwrap();
  }

  let result = builder()
    .track_url("https://track.example.net")
    .identity_file(&identity_file)
    .storage_dir(dir.path().join("storage"))
    .build()
    .await;
  assert!(matches!(
    result,
    Err(BuildError::Identity(
      crate::identity::IdentityError::InvalidFormat
    ))
  ));
  assert_eq!(std::fs::read(&identity_file).unwrap(), b"broken");
}

#[tokio::test]
async fn upload_concurrency_is_independent_and_bounded() {
  for count in [0, 17, usize::MAX] {
    assert!(matches!(
      builder().max_upload_peers(count).build().await,
      Err(BuildError::InvalidUploadPeers)
    ));
  }
  let dir = tempfile::tempdir().unwrap();
  let make = || {
    builder()
      .track_url("http://127.0.0.1:1")
      .identity_file(dir.path().join("upload-identity"))
      .storage_dir(dir.path().join("upload-storage"))
  };
  let default = make().build().await.unwrap();
  assert_eq!(default.uploads.available_permits(), 4);
  for count in [1, 16] {
    let client = make()
      .max_upload_peers(count)
      .max_download_peers(2)
      .build()
      .await
      .unwrap();
    assert_eq!(client.uploads.available_permits(), count);
    assert_eq!(client.max_download_peers, 2);
  }
}

#[tokio::test]
async fn upload_rate_defaults_to_unlimited_and_rejects_zero() {
  assert!(matches!(
    builder().upload_rate_limit(0).build().await,
    Err(BuildError::InvalidUploadRate)
  ));
  let dir = tempfile::tempdir().unwrap();
  let make = || {
    builder()
      .track_url("http://127.0.0.1:1")
      .identity_file(dir.path().join("rate-identity"))
      .storage_dir(dir.path().join("rate-storage"))
  };
  assert!(make().build().await.unwrap().upload_rate.is_none());
  for rate in [1, 10 * 1024 * 1024, u64::MAX] {
    assert!(
      make()
        .upload_rate_limit(rate)
        .build()
        .await
        .unwrap()
        .upload_rate
        .is_some()
    );
  }
}

#[tokio::test]
async fn source_read_limit_is_independent_and_validated() {
  for count in [0, 17, usize::MAX] {
    assert!(matches!(
      builder().max_upload_reads(count).build().await,
      Err(BuildError::InvalidUploadReads)
    ));
  }
  let dir = tempfile::tempdir().unwrap();
  let make = || {
    builder()
      .track_url("http://127.0.0.1:1")
      .identity_file(dir.path().join("identity-reads"))
      .storage_dir(dir.path().join("reads"))
  };
  assert_eq!(
    make()
      .build()
      .await
      .unwrap()
      .upload_reads
      .available_permits(),
    4
  );
  for count in [1, 16] {
    let client = make()
      .max_upload_reads(count)
      .max_upload_peers(2)
      .max_download_peers(3)
      .build()
      .await
      .unwrap();
    assert_eq!(client.upload_reads.available_permits(), count);
    assert_eq!(client.uploads.available_permits(), 2);
    assert_eq!(client.max_download_peers, 3);
  }
  let client = make().max_upload_reads(1).build().await.unwrap();
  let path = dir.path().join("source");
  std::fs::write(&path, [7; 1024]).unwrap();
  let first = client.prepare_file(&path).await.unwrap();
  let second = client.prepare_file(&path).await.unwrap();
  let held = first.read_permit().await;
  assert!(
    tokio::time::timeout(std::time::Duration::from_millis(10), second.read_permit())
      .await
      .is_err()
  );
  drop(held);
  let held = second.read_permit().await;
  assert_eq!(client.upload_reads.available_permits(), 0);
  drop(held);
  assert_eq!(client.upload_reads.available_permits(), 1);
}

#[tokio::test]
async fn peer_query_limit_defaults_to_spare_candidates_and_is_independent() {
  let dir = tempfile::tempdir().unwrap();
  let make = || {
    builder()
      .track_url("http://127.0.0.1:1")
      .identity_file(dir.path().join("identity"))
      .storage_dir(dir.path().join("storage"))
  };
  for count in [0, 51, usize::MAX] {
    assert!(matches!(
      make().peer_query_limit(count).build().await,
      Err(BuildError::InvalidPeerQueryLimit)
    ));
  }
  assert_eq!(make().build().await.unwrap().peer_query_limit, 8);
  for concurrent in [1, 4, 16] {
    let client = make().max_download_peers(concurrent).build().await.unwrap();
    assert_eq!(client.peer_query_limit, concurrent * 2);
  }
  for limit in [1, 50] {
    let client = make()
      .max_download_peers(4)
      .peer_query_limit(limit)
      .build()
      .await
      .unwrap();
    assert_eq!(client.peer_query_limit, limit);
    assert_eq!(client.max_download_peers, 4);
  }
}

#[tokio::test]
async fn upload_cache_budget_is_client_wide_and_bounded() {
  let dir = tempfile::tempdir().unwrap();
  let make = || {
    builder()
      .track_url("http://127.0.0.1:1")
      .identity_file(dir.path().join("identity"))
      .storage_dir(dir.path().join("storage"))
  };
  for bytes in [256 * 1024 * 1024 + 1, usize::MAX] {
    assert!(matches!(
      make().upload_cache_bytes(bytes).build().await,
      Err(BuildError::InvalidUploadCacheBytes)
    ));
  }
  assert_eq!(
    make().build().await.unwrap().upload_cache.capacity(),
    16 * 1024 * 1024
  );
  for bytes in [0, 1, 256 * 1024 * 1024] {
    assert_eq!(
      make()
        .upload_cache_bytes(bytes)
        .build()
        .await
        .unwrap()
        .upload_cache
        .capacity(),
      bytes
    );
  }
}
