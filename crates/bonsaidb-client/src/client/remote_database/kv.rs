use async_trait::async_trait;
use bonsaidb_core::{
    custom_api::CustomApi,
    kv::Kv,
    networking::{DatabaseRequest, DatabaseResponse, Request, Response},
};

#[async_trait]
impl<A> Kv for super::RemoteDatabase<A>
where
    A: CustomApi,
{
    async fn execute_key_operation(
        &self,
        op: bonsaidb_core::kv::KeyOperation,
    ) -> Result<bonsaidb_core::kv::Output, bonsaidb_core::Error> {
        match self
            .client
            .send_request(Request::Database {
                database: self.name.to_string(),
                request: DatabaseRequest::ExecuteKeyOperation(op),
            })
            .await?
        {
            Response::Database(DatabaseResponse::KvOutput(output)) => Ok(output),
            Response::Error(err) => Err(err),
            other => Err(bonsaidb_core::Error::Networking(
                bonsaidb_core::networking::Error::UnexpectedResponse(format!("{:?}", other)),
            )),
        }
    }
}
