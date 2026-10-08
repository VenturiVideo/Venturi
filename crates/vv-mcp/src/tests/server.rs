use super::*;
use rmcp::ClientHandler;
use rmcp::model::{CallToolRequestParams, ClientConfig};

#[derive(Clone, Default)]
struct Client;

impl ClientHandler for Client {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::default()
    }
}

fn text(result: &CallToolResult) -> &str {
    result.content[0]
        .as_text()
        .map(|t| t.text.as_str())
        .unwrap()
}

#[tokio::test]
async fn tools_are_listed_with_schemas_and_errors_are_tool_errors() {
    let (handle, inbox) = channel(|| {});
    let host = std::thread::spawn(move || {
        run_headless(Session::default(), inbox);
    });
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move {
        let service = VenturiServer::new(handle).serve(server_io).await?;
        service.waiting().await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });
    let client = Client.serve(client_io).await.unwrap();

    let info = client.peer_info().unwrap();
    assert_eq!(info.server_info.as_ref().unwrap().name, "venturi");
    let tools = client.list_all_tools().await.unwrap();
    assert_eq!(tools.len(), 40);
    let delete_ranges = tools.iter().find(|t| t.name == "delete_ranges").unwrap();
    assert!(delete_ranges.input_schema.contains_key("properties"));

    let project = client
        .call_tool(CallToolRequestParams::new("get_project"))
        .await
        .unwrap();
    assert_eq!(project.is_error, Some(false));
    assert!(text(&project).contains("\"timelines\": []"));

    let failed = client
        .call_tool(
            CallToolRequestParams::new("get_timeline").with_arguments(
                serde_json::json!({ "timeline_id": "5" })
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(failed.is_error, Some(true));
    assert_eq!(text(&failed), "unknown timeline id \"5\"");

    client.cancel().await.unwrap();
    let _ = server.await;
    host.join().unwrap();
}
