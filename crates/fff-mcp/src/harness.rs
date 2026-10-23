use rmcp::model::{ClientRequest, GetMeta, Implementation, JsonRpcMessage};
use rmcp::service::{RoleServer, RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::Transport;

pub(crate) struct HarnessTransport<T> {
    inner: T,
    legacy_client: Option<(String, Implementation)>,
}

impl<T> HarnessTransport<T> {
    pub(crate) fn new(inner: T) -> Self {
        Self {
            inner,
            legacy_client: None,
        }
    }
}

impl<T: Transport<RoleServer>> Transport<RoleServer> for HarnessTransport<T> {
    type Error = T::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        self.inner.send(item)
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleServer>> {
        let message = self.inner.receive().await?;
        if let JsonRpcMessage::Request(request) = &message {
            if let ClientRequest::InitializeRequest(initialize) = &request.request {
                self.legacy_client = Some((
                    initialize.params.protocol_version.to_string(),
                    initialize.params.client_info.clone(),
                ));
            }
            let host = host_context(&request.request, self.legacy_client.as_ref());
            if matches!(
                request.request,
                ClientRequest::InitializeRequest(_) | ClientRequest::DiscoverRequest(_)
            ) {
                tracing::info!(method = request.request.method(), host = %host, "MCP client context");
            } else {
                tracing::debug!(method = request.request.method(), request_id = ?request.id, host = %host, "MCP request context");
            }
        }
        Some(message)
    }

    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        self.inner.close()
    }
}

fn host_context(
    request: &ClientRequest,
    legacy_client: Option<&(String, Implementation)>,
) -> serde_json::Value {
    let meta = request.get_meta();
    let protocol = meta.protocol_version();
    // Stateless requests must never inherit the identity of another caller.
    let legacy_client = legacy_client.filter(|_| protocol.is_none());
    let client = meta
        .client_info()
        .or_else(|| legacy_client.map(|(_, client)| client.clone()));
    let mut host = serde_json::Map::new();
    if let Some(protocol) = protocol
        .map(|version| version.to_string())
        .or_else(|| legacy_client.map(|(version, _)| version.clone()))
    {
        host.insert("protocol_version".into(), protocol.into());
    }
    if let Some(client) = client {
        host.insert("client_name".into(), client.name.into());
        host.insert("client_version".into(), client.version.into());
    }
    // Client-supplied diagnostics only: never dump arbitrary _meta or tool arguments.
    for (source, target) in [
        ("threadId", "thread_id"),
        ("sessionId", "session_id"),
        ("callId", "call_id"),
    ] {
        if let Some(value) = meta.get(source).and_then(serde_json::Value::as_str) {
            host.insert(target.into(), value.into());
        }
    }
    if let Some(turn) = meta.get("x-codex-turn-metadata") {
        for key in [
            "thread_id",
            "parent_thread_id",
            "subagent_kind",
            "thread_source",
            "codex_version",
        ] {
            if let Some(value) = turn.get(key).and_then(serde_json::Value::as_str) {
                host.insert(key.into(), value.into());
            }
        }
    }
    host.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn caller_context_is_request_scoped_and_allowlisted() {
        let legacy = ("2025-06-18".into(), Implementation::new("legacy-host", "1"));
        for (metadata, expected) in [
            (
                json!({"threadId": "root", "sessionId": "session"}),
                json!({
                    "protocol_version": "2025-06-18", "client_name": "legacy-host",
                    "client_version": "1", "thread_id": "root", "session_id": "session"
                }),
            ),
            (
                json!({
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {},
                    "io.modelcontextprotocol/clientInfo": {"name": "new-host", "version": "2"},
                    "threadId": "child", "callId": "call",
                    "x-codex-turn-metadata": {
                        "thread_id": "child", "parent_thread_id": "root",
                        "subagent_kind": "thread_spawn", "thread_source": "subagent",
                        "codex_version": "test", "api_key": "must-not-be-logged"
                    },
                    "authorization": "must-not-be-logged"
                }),
                json!({
                    "protocol_version": "2026-07-28", "client_name": "new-host",
                    "client_version": "2", "thread_id": "child", "call_id": "call",
                    "parent_thread_id": "root", "subagent_kind": "thread_spawn",
                    "thread_source": "subagent", "codex_version": "test"
                }),
            ),
            (
                json!({
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {},
                    "threadId": 42, "x-codex-turn-metadata": {"parent_thread_id": false}
                }),
                json!({"protocol_version": "2026-07-28"}),
            ),
        ] {
            let request = serde_json::from_value(json!({
                "method": "tools/list", "params": {"_meta": metadata}
            }))
            .unwrap();
            assert_eq!(host_context(&request, Some(&legacy)), expected);
        }
    }
}
