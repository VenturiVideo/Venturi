use super::*;
use crate::{channel, run_headless};
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::{ClientHandler, ServiceExt};
use vv_media::test_support::TempDir;
use vv_session::Session;

#[derive(Clone, Default)]
struct Client;

impl ClientHandler for Client {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::default()
    }
}

fn test_dir(name: &str) -> TempDir {
    TempDir::new(&format!("vv-mcp-socket-{name}"))
}

fn wait_for(what: &str, condition: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !condition() {
        assert!(std::time::Instant::now() < deadline, "{what}");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[tokio::test]
async fn clients_connect_through_the_socket_and_are_counted() {
    let dir = test_dir("serve");
    let (handle, inbox) = channel(|| {});
    let host = std::thread::spawn(move || {
        run_headless(Session::default(), inbox);
    });
    let server = SocketServer::start_at(handle, dir.join("mcp-1.sock")).unwrap();
    let path = server.path().to_owned();
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let stream = tokio::net::UnixStream::connect(&path).await.unwrap();
    let client = Client.serve(stream).await.unwrap();
    wait_for("the client is counted", || server.clients() == 1);
    let project = client
        .call_tool(CallToolRequestParams::new("get_project"))
        .await
        .unwrap();
    assert_eq!(project.is_error, Some(false));

    client.cancel().await.unwrap();
    wait_for("the client is gone", || server.clients() == 0);
    drop(server);
    assert!(!path.exists(), "the socket is removed with the server");
    host.join().unwrap();
}

#[test]
fn live_sockets_drop_the_leftovers_of_dead_editors() {
    let dir = test_dir("live");
    let _alive = std::os::unix::net::UnixListener::bind(dir.join("mcp-10.sock")).unwrap();
    drop(std::os::unix::net::UnixListener::bind(dir.join("mcp-20.sock")).unwrap());
    std::fs::write(dir.join("notes.txt"), "not a socket").unwrap();

    let live = live_sockets(&dir);

    assert_eq!(live, vec![(10, dir.join("mcp-10.sock"))]);
    assert!(!dir.join("mcp-20.sock").exists());
    assert!(dir.join("notes.txt").exists());
}
