use super::*;

fn source(directory: &std::path::Path, bytes: &[u8]) -> PreparedFile {
  let path = directory.join("source.bin");
  std::fs::write(&path, bytes).unwrap();
  PreparedFile::open(path, "http://localhost".into()).unwrap()
}

#[tokio::test]
async fn slow_upload_exceeds_old_block_deadline_without_spurious_retries() {
  let directory = tempfile::tempdir().unwrap();
  let bytes = vec![7; 4096];
  let prepared = source(directory.path(), &bytes);
  let seed = prepared.seed().clone();
  let (download, upload) = crate::peer::test_pair(seed.info_hash()).await;
  let mut context = UploadContext::unmanaged();
  context.rate = Some(Arc::new(crate::upload_rate::UploadRate::new(128)));
  let now = tokio::time::Instant::now();
  let path = directory.path().join("slow-download");
  let (received, served) = tokio::time::timeout(Duration::from_secs(40), async {
    tokio::join!(
      download.download_file(seed, &path),
      upload.serve_file_scoped(Arc::new(prepared), context)
    )
  })
  .await
  .unwrap();
  received.unwrap();
  served.unwrap();
  assert!(now.elapsed() > BLOCK_DEADLINE);
  assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[tokio::test]
async fn retransmitted_piece_uses_the_same_rate_budget() {
  let (download, upload) = crate::peer::test_pair([9; 32]).await;
  let mut context = UploadContext::unmanaged();
  context.rate = Some(Arc::new(crate::upload_rate::UploadRate::new(1041)));
  let progress = crate::upload_progress::Progress::new();
  context.progress = Some(progress.connection("retransmit".into(), upload.peer_id().clone()));
  let bytes = vec![7; 1024];
  let now = tokio::time::Instant::now();
  let send = async {
    for attempt in 0..2 {
      send_pieces_limited(
        &*upload.data,
        Some((&*upload.control, &context)),
        0,
        &bytes,
        &[(0, 1024)],
        attempt == 1,
      )
      .await
      .unwrap();
    }
  };
  let receive = async {
    let mut count = 0;
    while count < 2 {
      if let Some(DataChannelEvent::OnMessage(message)) = download.data.poll().await {
        assert_eq!(Piece::decode(&message.data).unwrap().data, bytes);
        count += 1;
      }
    }
  };
  tokio::time::timeout(Duration::from_secs(5), async {
    tokio::join!(send, receive);
  })
  .await
  .unwrap();
  assert!(now.elapsed() >= Duration::from_secs(1));
  let snapshot = progress.snapshot();
  assert_eq!(snapshot.sent_bytes, 2082);
  assert_eq!(snapshot.retransmitted_bytes, 1041);
  assert_eq!(snapshot.connections[0].sent_bytes, 2082);
}

#[tokio::test]
async fn paced_large_upload_drains_buffer_notifications() {
  let directory = tempfile::tempdir().unwrap();
  let bytes = vec![19; 768 * 1024];
  let prepared = source(directory.path(), &bytes);
  let seed = prepared.seed().clone();
  let (download, upload) = crate::peer::test_pair(seed.info_hash()).await;
  let mut context = UploadContext::unmanaged();
  context.rate = Some(Arc::new(crate::upload_rate::UploadRate::new(256 * 1024)));
  let path = directory.path().join("paced-large");
  let (received, served) = tokio::time::timeout(Duration::from_secs(10), async {
    tokio::join!(
      download.download_file(seed, &path),
      upload.serve_file_scoped(Arc::new(prepared), context)
    )
  })
  .await
  .unwrap();
  received.unwrap();
  served.unwrap();
  assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[tokio::test]
async fn failed_data_channel_send_does_not_increment_upload_bytes() {
  let (_download, upload) = crate::peer::test_pair([9; 32]).await;
  let progress = crate::upload_progress::Progress::new();
  let mut context = UploadContext::unmanaged();
  context.progress = Some(progress.connection("closed".into(), upload.peer_id().clone()));
  let data = upload.data.clone();
  let control = upload.control.clone();
  upload.close().await.unwrap();
  assert!(
    send_pieces_limited(
      &*data,
      Some((&*control, &context)),
      0,
      &[7; 1024],
      &[(0, 1024)],
      false
    )
    .await
    .is_err()
  );
  assert_eq!(progress.snapshot().sent_bytes, 0);
  assert_eq!(progress.snapshot().connections[0].sent_bytes, 0);
}

#[tokio::test]
async fn queued_source_read_is_cancelable_without_spawning_or_reserving_capacity() {
  let directory = tempfile::tempdir().unwrap();
  let reads = Arc::new(tokio::sync::Semaphore::new(1));
  let prepared = Arc::new(source(directory.path(), &[3; 1024]).with_read_limit(reads.clone()));
  let held = reads.clone().acquire_owned().await.unwrap();
  let context = UploadContext::unmanaged();
  assert!(
    tokio::time::timeout(
      Duration::from_millis(10),
      context.read_block(prepared.clone(), 0)
    )
    .await
    .is_err()
  );
  tokio::time::timeout(Duration::from_secs(1), context.scope.idle())
    .await
    .unwrap();
  assert_eq!(reads.available_permits(), 0);
  drop(held);
  let block = tokio::time::timeout(Duration::from_secs(1), context.read_block(prepared, 0))
    .await
    .unwrap()
    .unwrap();
  assert_eq!(&*block.bytes, &vec![3; 1024]);
  assert_eq!(reads.available_permits(), 1);
}
