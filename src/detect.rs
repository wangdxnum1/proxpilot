//! Verified kernel backend; client discovery is extended in clients.rs.
use crate::{
    client_config::ClientKind,
    core_api::{ApiEndpoint, CoreApi},
    Args,
};

#[derive(Clone)]
pub struct Backend {
    pub kind: Option<ClientKind>,
    pub client: String,
    pub api: ApiEndpoint,
    pub secret: Option<String>,
    pub proxy: String,
    pub source: String,
    pub version: Option<String>,
    pub alternatives: Vec<(String, String)>,
}

pub fn detect(api: &CoreApi, args: &Args) -> Result<Backend, String> {
    crate::clients::resolve_backend(args, api, &crate::clients::DiscoveryPaths::current()?)
}
