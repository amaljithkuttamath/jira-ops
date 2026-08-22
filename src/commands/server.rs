use crate::client::{JiraClient, JiraTransport};
use crate::error::AppError;
use crate::model::{JiraServerInfo, ServerInfo};
use crate::output::SuccessEnvelope;

pub fn server_info<T: JiraTransport>(
    client: &JiraClient<T>,
) -> Result<SuccessEnvelope<ServerInfo>, AppError> {
    let response: JiraServerInfo = client.get_json_exact("/rest/api/3/serverInfo", 200)?;
    Ok(SuccessEnvelope::new(response.into()))
}
