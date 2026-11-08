use std::os::fd::AsRawFd;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

// Enter through the production artifact adapter and atomic installer. Listing
// and CLI argument parsing are bypassed to isolate a reset during the body.
#[tokio::test]
async fn reset_mid_stream_exhausts_retries_without_publishing_artifact() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for _ in 0..3 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(socket.read_u8().await.unwrap());
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\nConnection: close\r\n\r\nPK\x03\x04partial")
                .await
                .unwrap();
            // Allow headers and a body prefix to arrive before the abortive
            // close. Correctness does not depend on how many bytes arrive.
            tokio::time::sleep(Duration::from_millis(50)).await;
            let linger = libc::linger {
                l_onoff: 1,
                l_linger: 0,
            };
            // Safety: socket owns the live descriptor; linger has the type and
            // size required by SO_LINGER and lives through this call.
            let result = unsafe {
                libc::setsockopt(
                    socket.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_LINGER,
                    std::ptr::from_ref(&linger).cast(),
                    std::mem::size_of_val(&linger) as libc::socklen_t,
                )
            };
            assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
            drop(socket);
        }
    });
    let client = crate::client::Client::for_base_url(
        url::Url::parse(&format!("http://{address}/api/v1/")).unwrap(),
        "test-token".into(),
    )
    .unwrap();
    let artifact = serde_json::from_value(serde_json::json!({"id": 4, "name": "binary"})).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("binary.zip");
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let bytes =
            crate::api::workflow::download_artifact(&client, "o", "r", 5, &artifact).await?;
        crate::download::write_atomic(&destination, &bytes)
    })
    .await
    .expect("download must terminate");
    let error = result.unwrap_err().to_string();
    assert!(error.contains("after 3 attempts"), "{error}");
    assert!(error.contains("expected 100000 bytes"), "{error}");
    assert!(!destination.exists());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("server must finish all three attempts")
        .unwrap();
}
