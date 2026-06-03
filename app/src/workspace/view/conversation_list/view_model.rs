use crate::ai::agent::conversation::AIConversationId;
use crate::ai::agent_conversations_model::{
    AgentConversationsModel, AgentConversationsModelEvent, AgentManagementFilters, ArtifactFilter,
    ConversationOrTask, CreatedOnFilter, CreatorFilter, OwnerFilter, SessionStatus, SourceFilter,
    StatusFilter,
};
use crate::ai::ambient_agents::AmbientAgentTaskId;
use crate::ai::cli_agent_session_scanner::{scan_cached, DiscoveredCLIAgentSession};
use crate::terminal::CLIAgent;
use fuzzy_match::match_indices_case_insensitive;
use warpui::{AppContext, Entity, ModelContext, ModelHandle, SingletonEntity};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ConversationOrTaskId {
    ConversationId(AIConversationId),
    TaskId(AmbientAgentTaskId),
    /// 第三方 CLI agent 的历史会话。
    CLIAgentSession {
        session_id: String,
        agent_type: CLIAgent,
    },
}

impl ConversationOrTaskId {
    pub fn conversation_id(&self) -> Option<AIConversationId> {
        match self {
            ConversationOrTaskId::ConversationId(id) => Some(*id),
            ConversationOrTaskId::TaskId(_) | ConversationOrTaskId::CLIAgentSession { .. } => None,
        }
    }

    pub fn task_id(&self) -> Option<AmbientAgentTaskId> {
        match self {
            ConversationOrTaskId::TaskId(id) => Some(*id),
            ConversationOrTaskId::ConversationId(_)
            | ConversationOrTaskId::CLIAgentSession { .. } => None,
        }
    }
}

pub struct ConversationListViewModelEvent;

#[derive(Clone, Debug)]
pub struct ConversationEntry {
    pub id: ConversationOrTaskId,
    pub highlight_indices: Vec<usize>,
}

pub struct ConversationListViewModel {
    conversations_model: ModelHandle<AgentConversationsModel>,
    cached_conversation_or_task_ids: Vec<ConversationOrTaskId>,
    filtered_items: Vec<ConversationEntry>,
    search_query: String,
    /// 已发现的第三方 CLI agent 会话。
    cli_agent_sessions: Vec<DiscoveredCLIAgentSession>,
}

impl Entity for ConversationListViewModel {
    type Event = ConversationListViewModelEvent;
}

impl ConversationListViewModel {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let conversations_model = AgentConversationsModel::handle(ctx);

        ctx.subscribe_to_model(&conversations_model, |me, event, ctx| {
            match event {
                AgentConversationsModelEvent::ConversationsLoaded
                | AgentConversationsModelEvent::TasksUpdated
                | AgentConversationsModelEvent::TaskManuallyOpened => {
                    me.refresh_cached_items(ctx);
                }
                AgentConversationsModelEvent::ConversationUpdated => {
                    ctx.emit(ConversationListViewModelEvent);
                }
                AgentConversationsModelEvent::ConversationArtifactsUpdated { .. } => {}
            }
        });

        // 非阻塞：首次调用触发后台扫描，立即返回空列表；后续调用返回缓存。
        let cli_agent_sessions = scan_cached();

        let mut model = Self {
            conversations_model,
            cached_conversation_or_task_ids: Vec::new(),
            filtered_items: Vec::new(),
            search_query: String::new(),
            cli_agent_sessions,
        };
        model.refresh_cached_items(ctx);
        model
    }

    fn refresh_cached_items(&mut self, ctx: &mut ModelContext<Self>) {
        // 尝试重新读取缓存——后台扫描可能已经完成
        let fresh_sessions = scan_cached();
        if !fresh_sessions.is_empty()
            && (self.cli_agent_sessions.is_empty()
                || self.cli_agent_sessions.len() != fresh_sessions.len()
                || self.cli_agent_sessions[0].id != fresh_sessions[0].id)
        {
            self.cli_agent_sessions = fresh_sessions;
        }

        let model = self.conversations_model.as_ref(ctx);
        let mut ids: Vec<ConversationOrTaskId> = model
            .get_tasks_and_conversations(
                &AgentManagementFilters {
                    owners: OwnerFilter::PersonalOnly,
                    status: StatusFilter::All,
                    source: SourceFilter::All,
                    created_on: CreatedOnFilter::All,
                    creator: CreatorFilter::All,
                    artifact: ArtifactFilter::All,
                    environment: Default::default(),
                    harness: Default::default(),
                },
                ctx,
            )
            .filter(|item| {
                item.get_session_status()
                    .is_none_or(|status| status == SessionStatus::Available)
            })
            .filter(|item| {
                let is_user_initiated = item.source().is_some_and(|s| s.is_user_initiated());
                let is_manually_opened = match item {
                    ConversationOrTask::Task(task) => model.is_task_manually_opened(&task.task_id),
                    ConversationOrTask::Conversation(_) => false,
                };
                is_user_initiated || is_manually_opened
            })
            .map(|item| match item {
                ConversationOrTask::Task(task) => ConversationOrTaskId::TaskId(task.task_id),
                ConversationOrTask::Conversation(conv) => {
                    ConversationOrTaskId::ConversationId(conv.nav_data.id)
                }
            })
            .collect();

        // 追加第三方 CLI agent 会话
        ids.extend(self.cli_agent_sessions.iter().map(|s| {
            ConversationOrTaskId::CLIAgentSession {
                session_id: s.id.clone(),
                agent_type: s.agent_type,
            }
        }));

        self.cached_conversation_or_task_ids = ids;
        self.apply_search_filter(ctx);
        ctx.emit(ConversationListViewModelEvent);
    }

    pub fn set_search_query(&mut self, query: String, ctx: &mut ModelContext<Self>) {
        if query == self.search_query {
            return;
        }
        self.search_query = query;
        self.apply_search_filter(ctx);
        ctx.emit(ConversationListViewModelEvent);
    }

    fn apply_search_filter(&mut self, ctx: &mut ModelContext<Self>) {
        let search_query = self.search_query.trim().to_lowercase();
        let conversations_model = self.conversations_model.as_ref(ctx);

        if search_query.is_empty() {
            self.filtered_items = self
                .cached_conversation_or_task_ids
                .iter()
                .map(|id| ConversationEntry {
                    id: id.clone(),
                    highlight_indices: vec![],
                })
                .collect();
        } else {
            let mut matched_items: Vec<(i64, ConversationEntry)> = self
                .cached_conversation_or_task_ids
                .iter()
                .filter_map(|id| {
                    let title = match id {
                        ConversationOrTaskId::TaskId(task_id) => {
                            conversations_model.get_task(task_id)?.title(ctx)
                        }
                        ConversationOrTaskId::ConversationId(conv_id) => {
                            conversations_model.get_conversation(conv_id)?.title(ctx)
                        }
                        ConversationOrTaskId::CLIAgentSession {
                            session_id, ..
                        } => self
                            .cli_agent_sessions
                            .iter()
                            .find(|s| &s.id == session_id)?
                            .title
                            .clone(),
                    };

                    match_indices_case_insensitive(&title, &search_query).map(|result| {
                        (
                            result.score,
                            ConversationEntry {
                                id: id.clone(),
                                highlight_indices: result.matched_indices,
                            },
                        )
                    })
                })
                .collect();

            matched_items.sort_by(|a, b| b.0.cmp(&a.0));
            self.filtered_items = matched_items.into_iter().map(|(_, item)| item).collect();
        }
    }

    pub fn unfiltered_item_count(&self) -> usize {
        self.cached_conversation_or_task_ids.len()
    }

    pub fn filtered_items(&self) -> &[ConversationEntry] {
        &self.filtered_items
    }

    /// 查找 Oz 对话/task。CLI agent 会话返回 None。
    pub fn get_item_by_id<'a>(
        &self,
        id: &ConversationOrTaskId,
        ctx: &'a AppContext,
    ) -> Option<ConversationOrTask<'a>> {
        let model = self.conversations_model.as_ref(ctx);
        match id {
            ConversationOrTaskId::TaskId(task_id) => model.get_task(task_id),
            ConversationOrTaskId::ConversationId(conv_id) => model.get_conversation(conv_id),
            ConversationOrTaskId::CLIAgentSession { .. } => None,
        }
    }

    /// 查找 CLI agent 会话数据。
    pub fn get_cli_agent_session(
        &self,
        id: &ConversationOrTaskId,
    ) -> Option<&DiscoveredCLIAgentSession> {
        match id {
            ConversationOrTaskId::CLIAgentSession { session_id, .. } => {
                self.cli_agent_sessions.iter().find(|s| &s.id == session_id)
            }
            _ => None,
        }
    }

    /// 判断 item 是否为 CLI agent 会话。
    pub fn is_cli_agent_session(id: &ConversationOrTaskId) -> bool {
        matches!(id, ConversationOrTaskId::CLIAgentSession { .. })
    }

    pub fn current_ids(&self) -> impl Iterator<Item = &ConversationOrTaskId> {
        self.filtered_items.iter().map(|item| &item.id)
    }
}
