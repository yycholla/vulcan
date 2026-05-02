use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use vulcan::extensions::api::SessionExtensionCtx;
use vulcan::extensions::{ExtensionRegistry, ExtensionStatus};
use vulcan::hooks::HookRegistry;
use vulcan::memory::SessionStore;
use vulcan::provider::Message;
use vulcan::tools::{ToolRegistry, details_from_tool_message};
use vulcan_ext_todo::TodoExtension;

#[tokio::test]
async fn todo_details_survive_session_end_then_session_start_replay() {
    let memory = Arc::new(SessionStore::in_memory());
    let session_id = "todo-e2e-session";

    let registry = ExtensionRegistry::new();
    registry.register_daemon_extension(Arc::new(TodoExtension));

    let hooks = HookRegistry::new();
    let mut tools = ToolRegistry::new();
    let ctx = SessionExtensionCtx::new(
        std::env::current_dir().expect("cwd"),
        session_id.to_string(),
        Arc::clone(&memory),
    );
    let (sessions, extension_tools) =
        registry.wire_daemon_extensions_into_runtime(ctx, &hooks, Some(&mut tools));
    assert_eq!(sessions, 1);
    assert_eq!(extension_tools, 3);
    assert!(tools.contains("todo_add"));
    assert!(tools.contains("todo_list"));
    assert!(tools.contains("todo_clear"));

    let add = tools
        .execute(
            "todo_add",
            r#"{"item":"buy milk"}"#,
            CancellationToken::new(),
        )
        .await
        .expect("todo_add executes");
    assert_eq!(
        add.details,
        Some(serde_json::json!({ "items": ["buy milk"] }))
    );

    let content = serde_json::json!({
        "output": add.output,
        "details": add.details,
        "media": add.media,
        "is_error": add.is_error,
    })
    .to_string();
    assert_eq!(
        details_from_tool_message(&content),
        Some(serde_json::json!({ "items": ["buy milk"] }))
    );
    memory
        .save_messages(
            session_id,
            &[
                Message::Assistant {
                    content: None,
                    tool_calls: None,
                    reasoning_content: None,
                },
                Message::Tool {
                    tool_call_id: "call_1".to_string(),
                    content,
                },
            ],
        )
        .expect("save session history");
    hooks.session_end(session_id, 1).await;

    let restarted_hooks = HookRegistry::new();
    let mut restarted_tools = ToolRegistry::new();
    let ctx = SessionExtensionCtx::new(
        std::env::current_dir().expect("cwd"),
        session_id.to_string(),
        Arc::clone(&memory),
    );
    registry.wire_daemon_extensions_into_runtime(ctx, &restarted_hooks, Some(&mut restarted_tools));
    restarted_hooks.session_start(session_id).await;

    let list = restarted_tools
        .execute("todo_list", r#"{}"#, CancellationToken::new())
        .await
        .expect("todo_list executes after replay");
    assert!(list.output.contains("buy milk"));
    assert_eq!(
        list.details,
        Some(serde_json::json!({ "items": ["buy milk"] }))
    );

    let meta = registry
        .get("todo")
        .expect("todo metadata remains registered");
    assert_eq!(meta.status, ExtensionStatus::Active);
}
