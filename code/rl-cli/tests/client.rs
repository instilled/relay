//! Client against an in-process daemon.

use rl_core::Home;
use rl_proto::methods::DaemonPing;
use rl_cli::client::{Client, NotRunning};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn ping_in_process() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path());
    let token = CancellationToken::new();
    let server = tokio::spawn(rl_daemon::serve(home.clone(), token.clone()));
    for _ in 0..50 {
        if home.daemon_socket().exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut c = Client::connect(&home).await.unwrap();
    let info = c.call::<DaemonPing>(()).await.unwrap();
    assert_eq!(info.home, tmp.path());
    token.cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn not_running_is_typed() {
    let tmp = tempfile::tempdir().unwrap();
    let err = match Client::connect(&Home::at(tmp.path())).await {
        Ok(_) => panic!("connected to nothing"),
        Err(e) => e,
    };
    assert!(err.downcast_ref::<NotRunning>().is_some(), "{err:?}");
}
