use anyhow::Result;

use crate::core::store::SharedStore;

/// Universal building block for all agent operations
///
/// Every node follows the `prep → exec → post` pipeline:
/// - `prep`: read from SharedStore, prepare inputs
/// - `exec`: pure computation (LLM calls, shell commands, file I/O)
/// - `post`: write results back to SharedStore
pub trait Node {
    type PrepRes: Clone;
    type ExecRes: Clone;

    /// Read from store and prepare inputs for execution
    async fn prep(&self, store: &SharedStore) -> Result<Self::PrepRes>;

    /// Execute the core operation
    async fn exec(&self, prep_res: Self::PrepRes) -> Result<Self::ExecRes>;

    /// Write execution results back to the store
    async fn post(
        &self,
        store: &mut SharedStore,
        prep_res: Self::PrepRes,
        exec_res: Self::ExecRes,
    ) -> Result<()>;

    /// Run the full pipeline: prep → exec → post
    async fn run(&self, store: &mut SharedStore) -> Result<Self::ExecRes> {
        let prep_res = self.prep(store).await?;
        let exec_res = self.exec(prep_res.clone()).await?;
        self.post(store, prep_res, exec_res.clone()).await?;
        Ok(exec_res)
    }
}
