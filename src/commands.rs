//! Shared node evidence consumed by terminal, JSON and browse presenters.
use crate::{mihomo::ProxyInfo, nodeinfo::BillingPolicy};
use serde_json::{json, Value};
pub fn endpoint(api: &crate::core_api::ApiEndpoint) -> String {
    match api {
        crate::core_api::ApiEndpoint::Http(url) => crate::ui::display_url(url),
        _ => api.to_string(),
    }
}
pub fn endpoint_text(value: &str) -> String {
    if reqwest::Url::parse(value).is_ok_and(|url| matches!(url.scheme(), "http" | "https")) {
        crate::ui::display_url(value)
    } else {
        value.to_string()
    }
}
pub fn node(name: &str, info: &ProxyInfo, policy: &BillingPolicy, current: bool) -> Value {
    let rate = policy.resolve(name);
    json!({"name":name,"protocol":info.ptype,"protocol_source":"kernel_api",
        "rate":rate.map(|r|r.0),"rate_source":rate.map(|r|if r.1 {"subscription_hint"}else{"name"}),
        "labels":crate::nodeinfo::name_labels(name),"labels_source":"name_unverified","current":current})
}
pub fn backend(be: &crate::Backend) -> Value {
    json!({"client":be.kind.map(|k|k.name()).unwrap_or("mihomo"),"version":be.version,
        "controller_type":match be.api {crate::core_api::ApiEndpoint::Http(_)=>"http",_=>"named_pipe"}})
}
