//! Verified kernel backend; client discovery is extended in clients.rs.
use crate::{Args, core_api::{ApiEndpoint, CoreApi}, client_config::ClientKind};

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
    let urls: Vec<String> = if let Some(url) = &args.api { vec![url.clone()] }
        else if args.detect { [9090,9097,9091,9094,19090,28090].iter().map(|p| format!("http://127.0.0.1:{}",p)).collect() }
        else { vec!["http://127.0.0.1:9090".into()] };
    let mut last_error = String::new();
    for url in urls {
        let info = crate::procinfo::identify_client(crate::checker::parse_addr(&url).1);
        let kind = match info.name.as_str() { "CuteCloud" => Some(ClientKind::CuteCloud), "Clash Verge" => Some(ClientKind::ClashVerge), _ => None };
        let mut be = Backend { kind, client: info.name, api: url.into(), secret: args.secret.clone(), proxy: String::new(), source: "API".into(), version: None, alternatives: vec![] };
        match crate::mihomo::get_version(&be, api) {
            Ok(version) => { be.version = Some(version); be.proxy = args.proxy.clone().map(Ok).unwrap_or_else(|| crate::mihomo::get_runtime_proxy(&be, api))?; return Ok(be); }
            Err(e) => last_error = e,
        }
    }
    Err(format!("未连接到目标内核：{}",last_error))
}
