use anyhow::Result;

use crate::core::store::SharedStore;

pub trait Node {
    type PrepRes: Clone;
    type ExecRes: Clone;

    async fn prep(&self, store: &SharedStore) -> Result<Self::PrepRes>;
    async fn exec(&self, prep_res: Self::PrepRes) -> Result<Self::ExecRes>;
    async fn post(
        &self,
        store: &mut SharedStore,
        prep_res: Self::PrepRes,
        exec_res: Self::ExecRes,
    ) -> Result<()>;

    async fn run(&self, store: &mut SharedStore) -> Result<Self::ExecRes> {
        let prep_res = self.prep(store).await?;
        let prep_res_clone = prep_res.clone();
        let exec_res = self.exec(prep_res).await?;
        self.post(store, prep_res_clone, exec_res.clone()).await?;
        Ok(exec_res)
    }
}
