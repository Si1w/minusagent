use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;
use tokio::time::Duration;

use super::{LEAD_NAME, TeammateEntry, TeammateManager, TeammateStatus, persist_roster};
use crate::config::tuning;
use crate::engine::agent::{CotOptions, cot_loop};
use crate::engine::store::{Config, Context, LLMConfig, Message, Role, SharedStore, SystemState};
use crate::frontend::{Channel, SilentChannel};
use crate::intelligence::Intelligence;
use crate::intelligence::manager::SharedAgents;
use crate::routing::protocol::ToolPolicy;
use crate::team::task::{BackgroundManager, TaskManager};
use crate::team::todo::TodoManager;

/// Common prefix for all forked teammate initial messages.
///
/// All teammates share this exact byte sequence at the start of their first
/// user message. Combined with an identical system prompt (per `agent_id`),
/// this enables LLM API byte-level prefix caching — the first teammate's
/// prompt is cached and subsequent teammates reuse it.
const FORK_PREFIX: &str = "Fork started — processing in background.";

/// Immutable spawn parameters for a teammate.
pub struct TeammateSpawn<'a> {
    pub name: &'a str,
    pub role: &'a str,
    pub prompt: &'a str,
    pub agent_id: &'a str,
}

struct OwnedTeammateSpawn {
    name: String,
    role: String,
    prompt: String,
    agent_id: String,
}

impl From<TeammateSpawn<'_>> for OwnedTeammateSpawn {
    fn from(value: TeammateSpawn<'_>) -> Self {
        Self {
            name: value.name.to_string(),
            role: value.role.to_string(),
            prompt: value.prompt.to_string(),
            agent_id: value.agent_id.to_string(),
        }
    }
}

struct TeammateRuntime {
    spec: OwnedTeammateSpawn,
    llm_config: LLMConfig,
    agents: SharedAgents,
    tasks: Option<TaskManager>,
    wake_rx: mpsc::Receiver<()>,
}

impl TeammateManager {
    /// Spawn a new teammate or re-awaken an idle one
    ///
    /// # Arguments
    ///
    /// * `spec` - Teammate identity and initial task description
    /// * `llm_config` - LLM configuration
    /// * `agents` - Shared agent registry
    /// * `tasks` - Shared task graph for autonomous claiming
    ///
    /// # Returns
    ///
    /// Status message.
    ///
    /// # Errors
    ///
    /// Returns error if teammate exists and is not idle.
    pub fn spawn(
        &self,
        spec: TeammateSpawn<'_>,
        llm_config: LLMConfig,
        agents: SharedAgents,
        tasks: Option<TaskManager>,
    ) -> Result<String> {
        let spec_owned = OwnedTeammateSpawn::from(spec);

        {
            let mut state = self.lock_state();
            let existing = state
                .members
                .iter()
                .position(|member| member.name == spec_owned.name);
            if let Some(index) = existing {
                if state.members[index].status == TeammateStatus::Idle {
                    if let Some(tx) = state.wake_txs.get(&spec_owned.name) {
                        let _ = tx.try_send(());
                    }
                    state.members[index].status = TeammateStatus::Working;
                    persist_roster(&state)?;
                    return Ok(format!("Teammate '{}' re-awakened", spec_owned.name));
                }
                let status = &state.members[index].status;
                return Err(anyhow::anyhow!(
                    "Teammate '{}' already exists (status: {status:?})",
                    spec_owned.name
                ));
            }
        }

        let (wake_tx, wake_rx) = mpsc::channel::<()>(8);

        {
            let mut state = self.lock_state();
            state.members.push(TeammateEntry {
                name: spec_owned.name.clone(),
                role: spec_owned.role.clone(),
                status: TeammateStatus::Working,
                agent_id: spec_owned.agent_id.clone(),
            });
            state.wake_txs.insert(spec_owned.name.clone(), wake_tx);
            persist_roster(&state)?;
        }

        let team = self.clone();
        let runtime = TeammateRuntime {
            spec: spec_owned,
            llm_config,
            agents,
            tasks,
            wake_rx,
        };
        let status = format!(
            "Spawned teammate '{}' (role: {})",
            runtime.spec.name, runtime.spec.role
        );

        tokio::spawn(async move {
            let teammate_name = runtime.spec.name.clone();
            if let Err(error) = teammate_loop(team.clone(), runtime).await {
                log::error!("Teammate '{teammate_name}' error: {error}");
            }
            team.set_status(&teammate_name, TeammateStatus::Shutdown);
        });

        Ok(status)
    }
}

fn build_teammate_store(
    spec: &OwnedTeammateSpawn,
    team: &TeammateManager,
    llm_config: LLMConfig,
    agents: SharedAgents,
    tasks: Option<TaskManager>,
) -> SharedStore {
    let (system_prompt, intelligence) =
        build_teammate_identity(&spec.agent_id, &agents, &llm_config);
    let denied_tools = agents
        .get(&spec.agent_id)
        .map(|agent| agent.denied_tools.clone())
        .unwrap_or_default();

    SharedStore {
        context: Context {
            system_prompt,
            history: vec![Message {
                role: Role::User,
                content: Some(build_fork_message(&spec.name, &spec.role, &spec.prompt)),
                tool_calls: None,
                tool_call_id: None,
            }],
        },
        state: SystemState {
            config: Config { llm: llm_config },
            intelligence,
            todo: TodoManager::new(),
            is_subagent: true,
            agents,
            tasks,
            background: BackgroundManager::new(),
            team: Some(team.clone()),
            team_name: Some(spec.name.clone()),
            worktrees: None,
            tool_policy: ToolPolicy::from_denied(&denied_tools),
            idle_requested: false,
            plan_mode: false,
            cron: None,
            read_file_state: HashMap::new(),
        },
    }
}

fn reinject_teammate_identity(store: &mut SharedStore, spec: &OwnedTeammateSpawn) {
    if store.context.history.len() > 3 {
        return;
    }

    store.context.history.insert(
        0,
        Message {
            role: Role::User,
            content: Some(format!(
                "<identity>You are teammate '{}' (role: {}). Continue your work.</identity>",
                spec.name, spec.role
            )),
            tool_calls: None,
            tool_call_id: None,
        },
    );
    store.context.history.insert(
        1,
        Message {
            role: Role::Assistant,
            content: Some(format!("I am {}. Continuing.", spec.name)),
            tool_calls: None,
            tool_call_id: None,
        },
    );
}

async fn run_teammate_cycle(
    store: &mut SharedStore,
    channel: &Arc<dyn Channel>,
    http: &reqwest::Client,
) -> Result<()> {
    store.state.idle_requested = false;
    cot_loop(
        store,
        channel,
        http,
        &CotOptions {
            max_turns: Some(tuning().agent.max_teammate_turns),
            nag_reminder: false,
            flush_on_done: false,
            interrupted: None,
        },
    )
    .await
    .map(|_| ())
}

fn notify_teammate_idle(team: &TeammateManager, name: &str) {
    team.set_status(name, TeammateStatus::Idle);
    let _ = team.bus.send(
        name,
        LEAD_NAME,
        &format!("{name} finished current task"),
        "status",
        None,
    );
}

/// Run a teammate's agent loop with inbox integration
///
/// Each cycle: `cot_loop` (with auto inbox drain) → idle → wait
/// for wake. Repeats until the wake channel is closed.
async fn teammate_loop(team: TeammateManager, mut runtime: TeammateRuntime) -> Result<()> {
    let mut store = build_teammate_store(
        &runtime.spec,
        &team,
        runtime.llm_config,
        runtime.agents,
        runtime.tasks,
    );
    let channel: Arc<dyn Channel> = Arc::new(SilentChannel);
    let http = reqwest::Client::new();

    loop {
        reinject_teammate_identity(&mut store, &runtime.spec);
        run_teammate_cycle(&mut store, &channel, &http).await?;

        if team.is_shutdown(&runtime.spec.name) {
            break;
        }

        notify_teammate_idle(&team, &runtime.spec.name);
        if !idle_poll(&runtime.spec.name, &team, &mut store, &mut runtime.wake_rx).await {
            break;
        }
        team.set_status(&runtime.spec.name, TeammateStatus::Working);
    }

    Ok(())
}

/// Poll for work during idle phase
///
/// Checks inbox and task board every 5s. Returns `true` if work
/// was found, `false` on timeout or channel close.
async fn idle_poll(
    name: &str,
    team: &TeammateManager,
    store: &mut SharedStore,
    wake_rx: &mut mpsc::Receiver<()>,
) -> bool {
    let tuning = tuning();
    let deadline =
        tokio::time::Instant::now() + Duration::from_secs(tuning.timeouts.idle_timeout_secs);

    loop {
        let poll = tokio::time::timeout(
            Duration::from_secs(tuning.timeouts.idle_poll_interval_secs),
            wake_rx.recv(),
        )
        .await;

        match poll {
            Ok(Some(())) => return true,
            Ok(None) => return false,
            Err(_) => {}
        }

        if append_inbox_messages(team, name, store) {
            return true;
        }

        if try_claim_unclaimed_task(name, store) {
            return true;
        }

        if tokio::time::Instant::now() >= deadline {
            return false;
        }
    }
}

fn append_inbox_messages(team: &TeammateManager, name: &str, store: &mut SharedStore) -> bool {
    let messages = team.bus().read_inbox(name);
    if messages.is_empty() {
        return false;
    }

    let inbox_json = serde_json::to_string(&messages).unwrap_or_default();
    store.context.history.push(Message {
        role: Role::User,
        content: Some(format!("<inbox>\n{inbox_json}\n</inbox>")),
        tool_calls: None,
        tool_call_id: None,
    });
    store.context.history.push(Message {
        role: Role::Assistant,
        content: Some("Noted inbox messages.".into()),
        tool_calls: None,
        tool_call_id: None,
    });
    true
}

fn try_claim_unclaimed_task(name: &str, store: &mut SharedStore) -> bool {
    if let Some(tasks) = &store.state.tasks
        && let Ok(unclaimed) = tasks.scan_unclaimed()
        && let Some(task) = unclaimed.first()
    {
        let task_id = task.id;
        let subject = task.subject.clone();
        if tasks.claim(task_id, name).is_ok() {
            store.context.history.push(Message {
                role: Role::User,
                content: Some(format!(
                    "<auto-claimed>Task #{task_id}: {subject}\nYou have been assigned this task. Work on it now.</auto-claimed>"
                )),
                tool_calls: None,
                tool_call_id: None,
            });
            return true;
        }
    }

    false
}

/// Build the teammate's system prompt and optional Intelligence.
///
/// Returns the **base agent identity only** — no teammate-specific context.
/// All teammates sharing the same `agent_id` produce a byte-identical system
/// prompt, enabling LLM KV cache prefix reuse across concurrent forks.
/// Teammate context (name, role) is injected into the initial user message
/// via [`build_fork_message`] instead.
fn build_teammate_identity(
    agent_id: &str,
    agents: &SharedAgents,
    llm_config: &LLMConfig,
) -> (String, Option<Intelligence>) {
    if let Some(config) = (!agent_id.is_empty())
        .then(|| agents.get(agent_id))
        .flatten()
    {
        let workspace_dir = if config.workspace_dir.is_empty() {
            None
        } else {
            Some(PathBuf::from(&config.workspace_dir))
        };

        let intelligence = workspace_dir.as_ref().map(|workspace| {
            Intelligence::new(
                workspace,
                &config.system_prompt,
                agent_id.to_string(),
                "team".into(),
                llm_config.model.clone(),
            )
        });

        let base_prompt = intelligence
            .as_ref()
            .map(Intelligence::build_prompt)
            .unwrap_or(config.system_prompt.clone());

        return (base_prompt, intelligence);
    }

    ("You are a helpful assistant.".into(), None)
}

/// Build the initial user message for a forked teammate.
///
/// Structure: `FORK_PREFIX` (shared) + teammate context + task prompt.
/// The shared prefix maximises byte-level cache hits when multiple
/// teammates are spawned from the same agent.
fn build_fork_message(name: &str, role: &str, task: &str) -> String {
    format!(
        "{FORK_PREFIX}\n\n\
         You are teammate '{name}' (role: {role}).\n\
         Use team_send to message other teammates or 'lead'.\n\
         Your inbox is checked automatically before each response.\n\n\
         ---\n\n\
         {task}"
    )
}
