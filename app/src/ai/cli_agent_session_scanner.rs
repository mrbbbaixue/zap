//! 第三方 CLI agent 会话发现与扫描
//!
//! 通过读取 agent 本地文件系统来发现历史会话，
//! 使这些会话可被列入"智能对话"列表。
//! 不依赖 Oz 内部数据模型。
//!
//! 使用静态缓存避免重复扫描。首次调用 [`scan_cached`] 在后台线程
//! 中执行扫描，后续调用直接返回缓存结果。

use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::terminal::CLIAgent;

const MAX_CLI_AGENT_SESSIONS: usize = 50;

// ---- 缓存 ----

/// 全局缓存：`None` = 尚未扫描，`Some(vec)` = 已扫描完成。
static SESSION_CACHE: OnceLock<Mutex<Option<Vec<DiscoveredCLIAgentSession>>>> = OnceLock::new();

/// 后台扫描是否正在执行。
static SCAN_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

fn cache() -> &'static Mutex<Option<Vec<DiscoveredCLIAgentSession>>> {
    SESSION_CACHE.get_or_init(|| Mutex::new(None))
}

/// 如果缓存未就绪，在后台线程启动扫描（幂等）。
/// 如果扫描已在执行，静默返回。
fn ensure_background_scan() {
    // 缓存已就绪 → 不需要启动
    if let Ok(guard) = cache().lock() {
        if guard.is_some() {
            return;
        }
    }

    // 尝试获取扫描令牌
    if SCAN_IN_PROGRESS.swap(true, Ordering::AcqRel) {
        return; // 已有其他线程在扫描
    }

    std::thread::spawn(move || {
        let sessions = scan_all_sync();
        *cache().lock().unwrap() = Some(sessions);
        SCAN_IN_PROGRESS.store(false, Ordering::Release);
    });
}

/// 返回缓存中的会话列表。如果尚未扫描完成，触发后台扫描并返回空列表。
pub fn scan_cached() -> Vec<DiscoveredCLIAgentSession> {
    ensure_background_scan();
    cache().lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_default()
}

/// 清空缓存，下次调用 [`scan_cached`] 时重新扫描。
pub fn invalidate_cache() {
    if let Some(cache) = SESSION_CACHE.get() {
        *cache.lock().unwrap() = None;
    }
    SCAN_IN_PROGRESS.store(false, Ordering::Release);
}

// ---- 数据模型 ----

/// 已发现的 CLI agent 会话元数据。
#[derive(Debug, Clone)]
pub struct DiscoveredCLIAgentSession {
    pub id: String,
    pub agent_type: CLIAgent,
    pub title: String,
    pub session_id: String,
    pub working_directory: Option<String>,
    pub last_updated: DateTime<Utc>,
}

impl DiscoveredCLIAgentSession {
    /// 构建 resume 命令字符串。
    /// 例如 Claude Code: `claude resume <session-id>`
    pub fn resume_command(&self) -> String {
        let cmd = self.agent_type.command_prefix();
        format!("{cmd} resume {}", self.session_id)
    }
}

// ---- Claude Code ----------

/// Claude Code 会话文件的 JSON 结构。
/// 只取需要的字段，忽略未知字段。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct ClaudeSessionFile {
    session_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    messages: Vec<serde_json::Value>,
    #[serde(default)]
    history: Vec<serde_json::Value>,
}

/// 扫描 `~/.claude/sessions/` 发现 Claude Code 会话。
fn scan_claude_sessions(claude_dir: &PathBuf) -> Vec<DiscoveredCLIAgentSession> {
    let sessions_dir = claude_dir.join("sessions");
    let dir = match std::fs::read_dir(&sessions_dir) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };

    let mut sessions = Vec::new();
    for entry in dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let file: ClaudeSessionFile = match serde_json::from_str(&content) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let session_id = match &file.session_id {
            Some(id) => id.clone(),
            None => {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
                    .unwrap_or_default()
            }
        };
        if session_id.is_empty() {
            continue;
        }

        // 提取标题：name > title > 第一条 user 消息的前 80 字符
        let title = file
            .name
            .clone()
            .or_else(|| file.title.clone())
            .or_else(|| {
                let all_messages = if !file.messages.is_empty() {
                    &file.messages
                } else {
                    &file.history
                };
                all_messages.iter().find_map(|m| {
                    let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("");
                    if role == "user" || role == "human" {
                        m.get("content")
                            .or_else(|| m.get("text"))
                            .and_then(|c| c.as_str())
                            .map(|s| {
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
            })
            .unwrap_or_else(|| crate::t!("conversation-untitled"));

        let last_updated = file
            .updated_at
            .as_deref()
            .and_then(|t| {
                chrono::DateTime::parse_from_rfc3339(t)
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc))
            })
            .unwrap_or_else(Utc::now);

        sessions.push(DiscoveredCLIAgentSession {
            id: format!("claude:{}", session_id),
            agent_type: CLIAgent::Claude,
            title,
            session_id,
            working_directory: file.project,
            last_updated,
        });
    }

    sessions.sort_by(|a, b| b.last_updated.cmp(&a.last_updated));
    sessions
}

/// 确定 Claude 配置目录路径。
pub(crate) fn claude_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    dirs::home_dir().map(|h| h.join(".claude"))
}

/// **同步**扫描所有已知 CLI agent 的会话（阻塞 I/O）。
/// 一般情况下推荐使用 [`scan_cached`] 以利用缓存和异步后台扫描。
pub fn scan_all_sync() -> Vec<DiscoveredCLIAgentSession> {
    let mut all = Vec::new();
    if let Some(dir) = claude_dir() {
        all.extend(scan_claude_sessions(&dir));
    }
    all.truncate(MAX_CLI_AGENT_SESSIONS);
    all
}

/// 同步扫描（兼容旧调用点）。等同于 `scan_cached()`。
#[deprecated(note = "请使用 scan_cached() 以获得异步后台扫描")]
pub fn scan_all() -> Vec<DiscoveredCLIAgentSession> {
    scan_cached()
}

/// 同步扫描并限制数量（兼容旧调用点）。等同于 `scan_cached()`。
#[deprecated(note = "请使用 scan_cached() 以获得异步后台扫描")]
pub fn scan_all_with_limit(_limit: usize) -> Vec<DiscoveredCLIAgentSession> {
    scan_cached()
}

#[cfg(test)]
#[path = "cli_agent_session_scanner_tests.rs"]
mod tests;
