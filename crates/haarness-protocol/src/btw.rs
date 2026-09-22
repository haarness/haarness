use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::RequestId;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BtwRequest {
    pub question: String,
    #[serde(default)]
    pub request_id: RequestId,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BtwResponse {
    pub answer: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}
