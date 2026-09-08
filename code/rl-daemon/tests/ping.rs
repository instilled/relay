use relay_core::Home;
use relay_proto::codec::{read_message, write_message};
use relay_proto::methods::DaemonInfo;
use relay_proto::rpc::{Id, Request, Response, RpcError};
use serde_json::json;
use std::time::Duration;
use tokio::io::BufReader;
use tokio::net::UnixStream;
use tokio_util::sync::CancellationToken;

/// Waits until something accepts on the socket, not merely until the file
/// exists — a stale file from a previous daemon can be there from the start.
async fn wait_for(path: &std::path::Path) {
    for _ in 0..50 {
        if UnixStream::connect(path).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("nothing listening at {}", path.display());
}

#[tokio::test]
async fn ping_then_shutdown_removes_socket() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path());
    let token = CancellationToken::new();
    let server = tokio::spawn(rl_daemon::serve(home.clone(), token.clone()));
    wait_for(&home.daemon_socket()).await;
    assert!(home.daemon_pid().exists());

    let stream = UnixStream::connect(home.daemon_socket()).await.unwrap();
    let (rd, mut wr) = stream.into_split();
    let mut rd = BufReader::new(rd);

    write_message(&mut wr, &Request::new(Id::Num(1), "daemon.ping", None))
        .await
        .unwrap();
    let res: Response = read_message(&mut rd).await.unwrap().unwrap();
    assert_eq!(res.id, Id::Num(1));
    let info: DaemonInfo = serde_json::from_value(res.result.unwrap()).unwrap();
    assert_eq!(info.protocol, relay_proto::PROTOCOL_VERSION);
    assert_eq!(info.pid, std::process::id());
    assert_eq!(info.home, tmp.path());

    write_message(
        &mut wr,
        &Request::new(Id::Num(2), "nope.nothing", Some(json!({}))),
    )
    .await
    .unwrap();
    let res: Response = read_message(&mut rd).await.unwrap().unwrap();
    assert_eq!(res.error.unwrap().code, RpcError::METHOD_NOT_FOUND);

    write_message(&mut wr, &Request::new(Id::Num(3), "daemon.shutdown", None))
        .await
        .unwrap();
    let res: Response = read_message(&mut rd).await.unwrap().unwrap();
    assert!(res.error.is_none(), "shutdown should ack before exiting");

    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .expect("server exits")
        .unwrap()
        .unwrap();
    assert!(!home.daemon_socket().exists());
    assert!(!home.daemon_pid().exists());
}

#[tokio::test]
async fn stale_socket_file_is_replaced() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path());
    home.ensure_dirs().unwrap();
    // A crashed daemon leaves the socket file behind with nobody listening.
    drop(std::os::unix::net::UnixListener::bind(home.daemon_socket()).unwrap());
    assert!(home.daemon_socket().exists());
    let token = CancellationToken::new();
    let server = tokio::spawn(rl_daemon::serve(home.clone(), token.clone()));
    wait_for(&home.daemon_socket()).await;
    token.cancel();
    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn non_socket_file_at_socket_path_is_replaced() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path());
    home.ensure_dirs().unwrap();
    std::fs::write(home.daemon_socket(), b"junk").unwrap();
    let token = CancellationToken::new();
    let server = tokio::spawn(rl_daemon::serve(home.clone(), token.clone()));
    wait_for(&home.daemon_socket()).await;
    token.cancel();
    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
