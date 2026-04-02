use std::collections::HashSet;
use std::sync::Arc;

use tokio::sync::RwLock;
use std::time::Instant;

mod config;
mod core;
mod frontend;
mod intelligence;
mod logger;
mod resilience;
mod routing;
mod scheduler;

use crate::config::{AppConfig, tuning};
use crate::frontend::cli::Cli;
use crate::frontend::gateway::{AppState, Gateway};
use crate::frontend::Channel;
use crate::intelligence::manager::AgentManager;
use crate::routing::delivery::{BgOutputSink, OutboundSinks};
use crate::routing::router::{BindingRouter, BindingTable};

#[tokio::main]
async fn main() {
    scheduler::init_bg_output();

    let config = AppConfig::load();

    let mut mgr = AgentManager::new(config.primary_llm().model.clone());
    if let Some(ws) = &config.workspace_dir {
        mgr.discover_workspace(&ws.join(".agents"));
    }
    let mgr = std::sync::Arc::new(std::sync::RwLock::new(mgr));
    let mut table = BindingTable::new();
    if let Some(ws) = &config.workspace_dir {
        table.load_file(&ws.join("routes.json"));
    }
    let outbound = Arc::new(OutboundSinks::new(Arc::new(BgOutputSink)));
    let router = BindingRouter::new(table, mgr, &tuning().default_agent_id, outbound);

    let state = Arc::new(RwLock::new(AppState {
        router,
        sessions: HashSet::new(),
        start_time: Instant::now(),
    }));

    let gateway = Arc::new(Gateway::new(state, config).await);

    if std::env::args().any(|a| a == "--stdio") {
        if let Err(e) = frontend::stdio::run(gateway).await {
            eprintln!("stdio error: {e}");
        }
    } else {
        logger::TuiLogger::init();
        let cli: Arc<dyn Channel> = Arc::new(Cli::new());
        frontend::repl::run(gateway, cli).await;
    }
}