use std::future::Future;

use anyhow::Result;

use crate::engine::store::SharedStore;

/// Universal building block for all agent operations
///
/// Every node follows the `prep → exec → post` pipeline:
/// - `prep`: read from SharedStore, prepare inputs
/// - `exec`: pure computation (LLM calls, shell commands, file I/O)
/// - `post`: write results back to SharedStore
pub trait Node: Send + Sync {
    type PrepRes: Clone + Send;
    type ExecRes: Clone + Send;

    /// Read from store and prepare inputs for execution
    fn prep(
        &self,
        store: &SharedStore,
    ) -> impl Future<Output = Result<Self::PrepRes>> + Send;

    /// Execute the core operation
    fn exec(
        &self,
        prep_res: Self::PrepRes,
    ) -> impl Future<Output = Result<Self::ExecRes>> + Send;

    /// Write execution results back to the store
    fn post(
        &self,
        store: &mut SharedStore,
        prep_res: Self::PrepRes,
        exec_res: Self::ExecRes,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Run the full pipeline: prep → exec → post
    fn run(
        &self,
        store: &mut SharedStore,
    ) -> impl Future<Output = Result<Self::ExecRes>> + Send {
        async {
            let prep_res = self.prep(store).await?;
            let exec_res = self.exec(prep_res.clone()).await?;
            self.post(store, prep_res, exec_res.clone()).await?;
            Ok(exec_res)
        }
    }
}
