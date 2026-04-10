use std::fmt::Write;
use std::sync::Arc;

use anyhow::Result;

use crate::config::tuning;
use crate::engine::llm::LLMCall;
use crate::engine::node::Node;
use crate::engine::session::Session;
use crate::engine::store::{Message, Role, SharedStore};
use crate::frontend::{Channel, SilentChannel};
use crate::team::todo::TodoStatus;

const CLEARED_TOOL_MARKER: &str = "[Old tool result content cleared]";

#[derive(Clone, Copy)]
struct CompactPolicy {
    threshold: usize,
    summary_chars: usize,
    full_summary_chars: usize,
}

impl CompactPolicy {
    fn for_context_window(context_window: usize) -> Self {
        let tuning = tuning();
        Self {
            threshold: tuning.compaction.compact_threshold.apply_to(context_window),
            summary_chars: tuning
                .compaction
                .compact_summary_ratio
                .apply_to(context_window)
                * 4,
            full_summary_chars: tuning
                .compaction
                .full_compact_summary_ratio
                .apply_to(context_window)
                * 4,
        }
    }
}

pub(super) async fn compact_after_turn(
    session: &mut Session,
    total_tokens: Option<usize>,
    channel: &Arc<dyn Channel>,
) -> Result<()> {
    let Some(tokens) = total_tokens else {
        return Ok(());
    };

    let policy = CompactPolicy::for_context_window(session.store.state.config.llm.context_window);
    if tokens <= policy.threshold {
        return Ok(());
    }

    let cleared = micro_compact(&mut session.store);
    if cleared > 0 {
        channel
            .send(&format!("[compact L1] Cleared {cleared} old tool results"))
            .await;
    }

    if estimate_tokens(&session.store) <= policy.threshold {
        return Ok(());
    }

    if session.compact_failures < tuning().compaction.compact_max_failures {
        channel.send("[compact L2] Summarizing history...").await;
        match auto_compact(session).await {
            Ok(()) => session.compact_failures = 0,
            Err(error) => {
                session.compact_failures += 1;
                log::warn!(
                    "auto-compact failed ({}/{}): {error}",
                    session.compact_failures,
                    tuning().compaction.compact_max_failures,
                );
            }
        }
    }

    if estimate_tokens(&session.store) > policy.threshold {
        channel.send("[compact L3] Full compaction...").await;
        full_compact(session).await?;
    }

    Ok(())
}

pub(super) async fn compact_now(session: &mut Session) -> Result<(usize, usize)> {
    let before = session.store.context.history.len();
    micro_compact(&mut session.store);
    full_compact(session).await?;
    let after = session.store.context.history.len();
    Ok((before, after))
}

pub(super) fn estimate_tokens(store: &SharedStore) -> usize {
    store
        .context
        .history
        .iter()
        .map(|message| message.content.as_deref().unwrap_or("").len() / 4)
        .sum::<usize>()
        + store.context.system_prompt.len() / 4
}

fn micro_compact(store: &mut SharedStore) -> usize {
    let total = store.context.history.len();
    if total <= 4 {
        return 0;
    }

    let keep_recent = std::cmp::max(4, total / 5);
    let boundary = total - keep_recent;
    let mut cleared = 0;

    for message in &mut store.context.history[..boundary] {
        if message.role == Role::Tool
            && let Some(content) = &message.content
            && !content.starts_with(CLEARED_TOOL_MARKER)
        {
            message.content = Some(CLEARED_TOOL_MARKER.into());
            cleared += 1;
        }
    }

    if cleared > 0 {
        log::info!("micro-compact: cleared {cleared} tool results");
    }
    cleared
}

async fn auto_compact(session: &mut Session) -> Result<()> {
    let total = session.store.context.history.len();
    if total <= 4 {
        return Ok(());
    }

    let keep_count = std::cmp::max(4, total / 5);
    let compress_count = std::cmp::min(std::cmp::max(2, total / 2), total - keep_count);
    if compress_count < 2 {
        return Ok(());
    }

    let policy = CompactPolicy::for_context_window(session.store.state.config.llm.context_window);
    let old_text = format_messages_for_summary(&session.store.context.history[..compress_count]);
    let summary_prompt = format!(
        "CRITICAL: Respond with plain text ONLY. \
         Do NOT call any tools.\n\n\
         Summarize the following conversation concisely, \
         preserving key facts, decisions, and file paths. \
         Keep your summary under {} characters. \
         Output only the summary, no preamble.\n\n{old_text}",
        policy.summary_chars,
    );

    let summary_text = run_summarizer(session, &summary_prompt).await?;
    let mut compacted = vec![
        Message {
            role: Role::User,
            content: Some(format!("[Previous conversation summary]\n{summary_text}")),
            tool_calls: None,
            tool_call_id: None,
        },
        Message {
            role: Role::Assistant,
            content: Some("Understood, I have the context from our previous conversation.".into()),
            tool_calls: None,
            tool_call_id: None,
        },
    ];
    compacted.extend_from_slice(&session.store.context.history[compress_count..]);
    session.store.context.history = compacted;

    log::info!(
        "auto-compact: {total} -> {} messages",
        session.store.context.history.len(),
    );
    Ok(())
}

async fn full_compact(session: &mut Session) -> Result<()> {
    let total = session.store.context.history.len();
    if total <= 2 {
        return Ok(());
    }

    let policy = CompactPolicy::for_context_window(session.store.state.config.llm.context_window);
    let old_text = format_messages_for_summary(&session.store.context.history);
    let summary_prompt = format!(
        "CRITICAL: Respond with plain text ONLY. \
         Do NOT call any tools.\n\n\
         Summarize the following entire conversation, \
         preserving ALL key facts, decisions, file paths, \
         code changes, and current task state. \
         Keep your summary under {} characters. \
         Output only the summary, no preamble.\n\n{old_text}",
        policy.full_summary_chars,
    );

    let summary_text = run_summarizer(session, &summary_prompt).await?;
    let reinject = build_reinjection_context(&session.store);
    session.store.context.history = vec![
        Message {
            role: Role::User,
            content: Some(format!(
                "[Full conversation summary]\n{summary_text}{reinject}"
            )),
            tool_calls: None,
            tool_call_id: None,
        },
        Message {
            role: Role::Assistant,
            content: Some(
                "Understood. I have the full context from our conversation and will continue from here."
                    .into(),
            ),
            tool_calls: None,
            tool_call_id: None,
        },
    ];

    log::info!(
        "full-compact: {total} -> {} messages",
        session.store.context.history.len(),
    );
    Ok(())
}

async fn run_summarizer(session: &mut Session, prompt: &str) -> Result<String> {
    let original_history = std::mem::take(&mut session.store.context.history);
    let original_prompt = session.store.context.system_prompt.clone();

    session.store.context.system_prompt =
        "You are a conversation summarizer. Be concise and factual. NEVER call tools — respond with plain text only."
            .into();
    session.store.context.history = vec![Message {
        role: Role::User,
        content: Some(prompt.to_string()),
        tool_calls: None,
        tool_call_id: None,
    }];

    let llm = LLMCall {
        channel: Arc::new(SilentChannel),
        http: session.http.clone(),
    };
    let response = llm.run(&mut session.store).await;

    session.store.context.system_prompt = original_prompt;
    session.store.context.history = original_history;

    response.map(|reply| reply.content.unwrap_or_default())
}

fn build_reinjection_context(store: &SharedStore) -> String {
    let mut reinject = String::new();

    if !store.state.read_file_state.is_empty() {
        reinject.push_str("\n\n[Recently read files]\n");
        for path in store.state.read_file_state.keys() {
            let _ = writeln!(reinject, "- {path}");
        }
    }

    let active_items = store
        .state
        .todo
        .items
        .iter()
        .filter(|item| !matches!(item.status, TodoStatus::Completed))
        .collect::<Vec<_>>();
    if !active_items.is_empty() {
        reinject.push_str("\n[Active tasks]\n");
        for item in active_items {
            let _ = writeln!(reinject, "- [{:?}] {}", item.status, item.text);
        }
    }

    reinject
}

fn format_messages_for_summary(messages: &[Message]) -> String {
    let mut text = String::with_capacity(messages.len() * 200);
    for message in messages {
        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        if let Some(content) = &message.content {
            let _ = writeln!(text, "[{role}]: {content}");
        }
    }
    text
}
