use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use std::time::Instant;

mod core;
mod frontend;
mod intelligence;
mod logger;
mod routing;

use crate::frontend::cli::Cli;
use crate::frontend::gateway::{AppState, Gateway, ProviderConfig};
use crate::frontend::Channel;
use crate::intelligence::manager::AgentManager;
use crate::routing::router::{BindingRouter, BindingTable};

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    logger::TuiLogger::init();

    let provider = ProviderConfig::from_env();

    let mut mgr = AgentManager::new(provider.default_model.clone());
    if let Some(ws) = &provider.workspace_dir {
        mgr.discover_workspace(&ws.join(".agents"));
    }
    let mut table = BindingTable::new();
    if let Some(ws) = &provider.workspace_dir {
        table.load_file(&ws.join("routes.json"));
    }
    let router = BindingRouter::new(table, mgr, "mandeven");

    let state = Arc::new(RwLock::new(AppState {
        router,
        sessions: HashSet::new(),
        start_time: Instant::now(),
    }));

    let gateway = Arc::new(Gateway::new(state, provider));
    let cli: Arc<dyn Channel> = Arc::new(Cli::new());

    frontend::repl::run(gateway, cli).await;
}