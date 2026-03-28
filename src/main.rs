use std::collections::HashSet;
use std::sync::Arc;

use tokio::sync::RwLock;
use std::time::Instant;

mod core;
mod frontend;
mod intelligence;
mod logger;
mod resilience;
mod routing;
mod scheduler;

use crate::frontend::cli::Cli;
use crate::frontend::gateway::{AppState, Gateway, ProviderConfig};
use crate::frontend::Channel;
use crate::intelligence::manager::AgentManager;
use crate::routing::delivery::{BgOutputSink, OutboundSinks};
use crate::routing::router::{BindingRouter, BindingTable};

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    logger::TuiLogger::init();
    scheduler::init_bg_output();

    let provider = ProviderConfig::from_env();

    let mut mgr = AgentManager::new(provider.default_model.clone());
    if let Some(ws) = &provider.workspace_dir {
        mgr.discover_workspace(&ws.join(".agents"));
    }
    let mgr = std::sync::Arc::new(std::sync::RwLock::new(mgr));
    let mut table = BindingTable::new();
    if let Some(ws) = &provider.workspace_dir {
        table.load_file(&ws.join("routes.json"));
    }
    let outbound = Arc::new(OutboundSinks::new(Arc::new(BgOutputSink)));
    let router = BindingRouter::new(table, mgr, "mandeven", outbound);

    let state = Arc::new(RwLock::new(AppState {
        router,
        sessions: HashSet::new(),
        start_time: Instant::now(),
    }));

    let gateway = Arc::new(Gateway::new(state, provider).await);
    let cli: Arc<dyn Channel> = Arc::new(Cli::new());

    frontend::repl::run(gateway, cli).await;
}