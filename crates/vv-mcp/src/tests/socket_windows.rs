use super::*;
use crate::{channel, run_headless};
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::{ClientHandler, ServiceExt};
use vv_session::Session;

#[derive(Clone, Default)]
struct Client;

impl ClientHandler for Client {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::default()
    }
}

fn wait_for(what: &str, condition: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !condition() {
        assert!(std::time::Instant::now() < deadline, "{what}");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

// A pid no editor has: the test must not see, nor be seen by, a running Venturi.
fn test_pipe(pid: u32) -> PathBuf {
    socket_dir().join(format!("{}{}", pipe_prefix(), socket_name(pid)))
}

#[tokio::test]
async fn clients_connect_through_the_pipe_and_are_counted() {
    let (handle, inbox) = channel(|| {});
    let host = std::thread::spawn(move || {
        run_headless(Session::default(), inbox);
    });
    let server = SocketServer::start_at(handle, test_pipe(u32::MAX)).unwrap();

    let stream = tokio::net::windows::named_pipe::ClientOptions::new()
        .open(server.path())
        .unwrap();
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
    host.join().unwrap();
}

#[test]
fn live_sockets_list_the_pipes_being_served_only() {
    let pid = u32::MAX - 1;
    let path = test_pipe(pid);
    let (handle, _inbox) = channel(|| {});
    let server = SocketServer::start_at(handle, path.clone()).unwrap();
    assert!(live_sockets(&socket_dir()).contains(&(pid, path.clone())));

    drop(server);
    assert!(!live_sockets(&socket_dir()).contains(&(pid, path)));
}

#[test]
fn a_second_server_cannot_take_a_served_name() {
    let path = test_pipe(u32::MAX - 2);
    let (handle, _inbox) = channel(|| {});
    let _server = SocketServer::start_at(handle.clone(), path.clone()).unwrap();
    assert!(SocketServer::start_at(handle, path).is_err());
}

#[test]
fn the_bridge_accepts_a_pipe_this_user_serves_only_under_its_own_pid() {
    let pid = std::process::id();
    let path = test_pipe(u32::MAX - 3);
    let (handle, _inbox) = channel(|| {});
    let _server = SocketServer::start_at(handle, path.clone()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let pipe = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(&path)
            .unwrap();
        crate::pipe_security::check_server(&pipe, pid).unwrap();
        let posing = crate::pipe_security::check_server(&pipe, pid + 1).unwrap_err();
        assert_eq!(posing.kind(), std::io::ErrorKind::PermissionDenied);
    });
}
