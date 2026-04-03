use std::collections::HashSet;
use std::sync::Arc;

use tokio::sync::RwLock;
use std::time::Instant;

use minusagent::config::{AppConfig, tuning};
use minusagent::frontend::cli::Cli;
use minusagent::frontend::gateway::{AppState, Gateway};
use minusagent::frontend::repl;
use minusagent::frontend::stdio;
use minusagent::frontend::Channel;
use minusagent::intelligence::manager::AgentManager;
use minusagent::logger::TuiLogger;
use minusagent::routing::delivery::{BgOutputSink, OutboundSinks};
use minusagent::routing::router::{BindingRouter, BindingTable};
use minusagent::scheduler;

#[tokio::main]
async fn main() {
    scheduler::init_bg_output();

    let config = AppConfig::load();

    let mut mgr = AgentManager::new(config.primary_llm().model.clone());
    if let Some(ws) = &config.workspace_dir {
        mgr.discover_workspace(&ws.join(".agents"));
    }
    let mgr = Arc::new(std::sync::RwLock::new(mgr));
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
        if let Err(e) = stdio::run(gateway).await {
            eprintln!("stdio error: {e}");
        }
    } else {
        TuiLogger::init();
        let cli: Arc<dyn Channel> = Arc::new(Cli::new());
        repl::run(gateway, cli).await;
    }
}