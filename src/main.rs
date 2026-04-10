use minusagent::config::AppConfig;
use minusagent::runtime::AppRuntime;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = AppConfig::load();
    AppRuntime::from_env(config).await?.run().await
}
