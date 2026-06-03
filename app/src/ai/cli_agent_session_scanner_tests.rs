//! CLI agent session scanner tests.

use super::*;

#[test]
fn test_claude_dir_uses_env_var() {
    // 验证 CLAUDE_CONFIG_DIR 环境变量被优先使用
    std::env::set_var("CLAUDE_CONFIG_DIR", "/tmp/.claude-test");
    let dir = claude_dir();
    assert_eq!(dir, Some(std::path::PathBuf::from("/tmp/.claude-test")));
    std::env::remove_var("CLAUDE_CONFIG_DIR");
}

#[test]
fn test_claude_session_deserialize() {
    let json = r#"{
        "session_id": "test-123",
        "name": "Fix login bug",
        "updated_at": "2025-06-01T10:00:00Z",
        "project": "/home/user/project",
        "messages": [
            {"role": "user", "content": "Fix the login bug please"},
            {"role": "assistant", "content": "I've fixed it"}
        ]
    }"#;

    let file: ClaudeSessionFile = serde_json::from_str(json).unwrap();
    assert_eq!(file.session_id, Some("test-123".to_string()));
    assert_eq!(file.name, Some("Fix login bug".to_string()));
    assert_eq!(file.project, Some("/home/user/project".to_string()));
    assert_eq!(file.messages.len(), 2);
}

#[test]
fn test_claude_session_deserialize_without_name() {
    // 无 name 字段时，从 messages 提取标题
    let json = r#"{
        "session_id": "test-456",
        "updated_at": "2025-06-01T10:00:00Z",
        "messages": [
            {"role": "user", "content": "Refactor the database layer"}
        ]
    }"#;

    let file: ClaudeSessionFile = serde_json::from_str(json).unwrap();
    assert_eq!(file.session_id, Some("test-456".to_string()));
    assert!(file.name.is_none());
}

#[test]
fn test_claude_session_title_fallback_from_messages() {
    let json = r#"{
        "session_id": "test-789",
        "updated_at": "2025-06-01T10:00:00Z",
        "messages": [
            {"role": "user", "content": "Hello, can you help me debug this?"}
        ]
    }"#;

    let file: ClaudeSessionFile = serde_json::from_str(json).unwrap();
    // 测试标题提取：第一条 user 消息前 80 字符
    let title = file
        .name
        .or_else(|| {
            file.messages.iter().find_map(|m| {
                let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("");
                if role == "user" {
                    m.get("content").and_then(|c| c.as_str()).map(|s| {
                        if s.len() > 80 {
                            format!("{}…", &s[..80])
                        } else {
                            s.to_string()
                        }
                    })
                } else {
                    None
                }
            })
        });
    assert_eq!(title, Some("Hello, can you help me debug this?".to_string()));
}
