// SPDX-FileCopyrightText: 2025 Sven Shi
// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared DNS request lifecycle for server plugins.

use std::net::SocketAddr;
use std::sync::Arc;

use tracing::{Level, debug, event_enabled, warn};

use super::metrics::ServerMetrics;
use crate::core::context::DnsContext;
pub use crate::core::context::RequestMeta;
use crate::infra::network::ip::normalize_ipv4_mapped_socket_addr;
use crate::plugin::executor::{ExecStep, Executor};
use crate::proto::{Edns, EdnsCode, Message, Rcode};

#[derive(Debug)]
pub struct RequestHandle {
    pub entry_executor: Arc<dyn Executor>,
    /// Shared server metrics. `None` for internal/test handles that should not
    /// emit server-level metrics.
    pub(crate) metrics: Option<Arc<ServerMetrics>>,
    /// Strip the EDNS Client Subnet option from the request before it enters
    /// the pipeline and from the outbound response.
    pub(crate) disable_ecs: bool,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RequestExit {
    Completed,
    Controlled,
    Failed,
}

#[derive(Debug)]
#[allow(unused)]
pub struct RequestResult {
    pub request: Message,
    pub response: Message,
    pub exit: RequestExit,
}

impl RequestHandle {
    #[hotpath::measure]
    pub async fn handle_request(
        &self,
        mut msg: Message,
        src_addr: SocketAddr,
        meta: RequestMeta,
    ) -> RequestResult {
        let metrics_start = self.metrics.as_ref().map(|m| m.on_request_start());

        self.strip_request_ecs(&mut msg);
        let mut context = DnsContext::new(normalize_ipv4_mapped_socket_addr(src_addr), msg);
        self.apply_request_meta(&mut context, meta);

        // Ingress validation: a spoofed response (QR=1) or a non-query
        // opcode must not run the pipeline, touch the cache, or reach any
        // upstream; answer REFUSED directly instead.
        if context.request.message_type() != crate::proto::MessageType::Query
            || context.request.opcode() != crate::proto::Opcode::Query
        {
            debug!(
                "Dropping non-query ingress from {} (QR=1 or unsupported opcode), id {}",
                src_addr,
                context.request.id()
            );
            let mut response = context.request.response(Rcode::Refused);
            self.finalize_response(&context.request, &mut response);
            if let (Some(metrics), Some(start_ms)) = (self.metrics.as_ref(), metrics_start) {
                metrics.on_request_finish(start_ms, RequestExit::Controlled);
            }
            return RequestResult {
                request: context.request,
                response,
                exit: RequestExit::Controlled,
            };
        }

        // RFC 6891 6.1.3: an OPT with an unsupported EDNS version must be
        // answered with BADVERS instead of executing the query.
        if context
            .request
            .edns()
            .as_ref()
            .is_some_and(|edns| edns.version() > 0)
        {
            debug!(
                "Refusing unsupported EDNS version from {}, id {}",
                src_addr,
                context.request.id()
            );
            let mut response = context.request.response(Rcode::BADVERS);
            self.finalize_response(&context.request, &mut response);
            if let (Some(metrics), Some(start_ms)) = (self.metrics.as_ref(), metrics_start) {
                metrics.on_request_finish(start_ms, RequestExit::Controlled);
            }
            return RequestResult {
                request: context.request,
                response,
                exit: RequestExit::Controlled,
            };
        }

        if event_enabled!(Level::DEBUG) {
            debug!(
                "DNS request from {}, queries: {:?}, id: {}, edns: {:?}, nameservers: {:?}",
                &src_addr,
                context.request.questions(),
                context.request.id(),
                context.request.edns(),
                context.request.authorities()
            );
        }

        let exec_outcome = self
            .entry_executor
            .execute_with_next(&mut context, None)
            .await;
        let (mut response, exit) = match exec_outcome {
            Ok(step) => {
                let exit = match step {
                    ExecStep::Next => RequestExit::Completed,
                    ExecStep::Stop | ExecStep::Return => RequestExit::Controlled,
                };
                let response = context
                    .take_response()
                    .unwrap_or_else(|| self.build_empty_response(&context));
                (response, exit)
            }
            Err(e) => {
                warn!(
                    "Entry executor '{}' failed for source {} id {}: {}",
                    self.entry_executor.tag(),
                    src_addr,
                    context.request.id(),
                    e
                );
                (self.build_servfail_response(&context), RequestExit::Failed)
            }
        };

        self.finalize_response(&context.request, &mut response);

        if event_enabled!(Level::DEBUG) {
            debug!(
                "Sending response to {}, exit: {:?}, queries: {:?}, id: {}, edns: {:?}, answers: {:?}",
                &src_addr,
                exit,
                context.request.questions(),
                response.id(),
                response.edns(),
                response.answers()
            );
        }

        if let (Some(metrics), Some(start_ms)) = (self.metrics.as_ref(), metrics_start) {
            metrics.on_request_finish(start_ms, exit);
        }

        RequestResult {
            request: context.request,
            response,
            exit,
        }
    }

    #[inline]
    fn apply_request_meta(&self, context: &mut DnsContext, meta: RequestMeta) {
        context.set_request_meta(RequestMeta {
            server_name: meta.server_name.filter(|value| !value.is_empty()),
            url_path: meta.url_path.filter(|value| !value.is_empty()),
        });
    }

    #[inline]
    fn build_servfail_response(&self, context: &DnsContext) -> Message {
        self.build_base_response(context, Rcode::ServFail)
    }

    #[inline]
    fn build_empty_response(&self, context: &DnsContext) -> Message {
        self.build_base_response(context, Rcode::NoError)
    }

    #[inline]
    fn build_base_response(&self, context: &DnsContext, rcode: Rcode) -> Message {
        context.request().response(rcode)
    }

    /// Remove the EDNS Client Subnet option from an inbound query so plugins
    /// downstream never observe the client subnet.
    #[inline]
    fn strip_request_ecs(&self, request: &mut Message) {
        if !self.disable_ecs {
            return;
        }
        if let Some(edns) = request.edns_mut() {
            edns.remove(EdnsCode::Subnet);
        }
    }

    /// Apply server-level RFC fixes to every outbound response.
    fn finalize_response(&self, request: &Message, response: &mut Message) {
        response.set_recursion_available(true);

        if request.edns().is_some() && response.edns().is_none() {
            let mut edns = Edns::new();
            if let Some(req_edns) = request.edns() {
                edns.flags_mut().dnssec_ok = req_edns.flags().dnssec_ok;
            }
            response.set_edns(edns);
        }

        // A response must not echo a client subnet either, otherwise the
        // client learns which subnet upstream resolution used.
        if self.disable_ecs
            && let Some(edns) = response.edns_mut()
        {
            edns.remove(EdnsCode::Subnet);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::*;
    use crate::continue_next;
    use crate::infra::error::Result;
    use crate::plugin::Plugin;
    use crate::proto::{ClientSubnet, EdnsOption, Name, Question, RecordType};

    fn make_request(id: u16, qname: &str) -> Message {
        let mut request = Message::new();
        request.set_id(id);
        request.add_question(Question::new(
            Name::from_ascii(qname).expect("query name should be valid"),
            RecordType::A,
            crate::proto::DNSClass::IN,
        ));
        request
    }

    fn make_ecs_request(id: u16, qname: &str) -> Message {
        let mut request = make_request(id, qname);
        let mut edns = Edns::new();
        edns.insert(EdnsOption::Subnet(ClientSubnet::new(
            IpAddr::V4(Ipv4Addr::new(192, 0, 2, 42)),
            24,
            0,
        )));
        request.set_edns(edns);
        request
    }

    fn make_request_handle(executor: Arc<dyn Executor>) -> RequestHandle {
        make_request_handle_with_disable_ecs(executor, false)
    }

    fn make_request_handle_with_disable_ecs(
        executor: Arc<dyn Executor>,
        disable_ecs: bool,
    ) -> RequestHandle {
        RequestHandle {
            entry_executor: executor,
            metrics: None,
            disable_ecs,
        }
    }

    #[derive(Debug, Default, Clone, PartialEq, Eq)]
    struct ObservedMeta {
        server_name: Option<String>,
        url_path: Option<String>,
    }

    #[derive(Debug)]
    struct CaptureMetaExecutor {
        observed: Arc<Mutex<Option<ObservedMeta>>>,
    }

    #[async_trait]
    impl Plugin for CaptureMetaExecutor {
        fn tag(&self) -> &str {
            "capture_meta"
        }

        async fn init(&mut self, _context: &crate::plugin::PluginInitContext<'_>) -> Result<()> {
            Ok(())
        }

        async fn destroy(&self) -> Result<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl Executor for CaptureMetaExecutor {
        async fn execute(&self, context: &mut DnsContext) -> Result<ExecStep> {
            let observed = ObservedMeta {
                server_name: context.server_name().map(str::to_string),
                url_path: context.url_path().map(str::to_string),
            };
            self.observed
                .lock()
                .expect("meta capture lock should not be poisoned")
                .replace(observed);
            Ok(ExecStep::Next)
        }
    }

    #[derive(Debug)]
    struct PostResponseExecutor;

    #[async_trait]
    impl Plugin for PostResponseExecutor {
        fn tag(&self) -> &str {
            "post_response"
        }

        async fn init(&mut self, _context: &crate::plugin::PluginInitContext<'_>) -> Result<()> {
            Ok(())
        }

        async fn destroy(&self) -> Result<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl Executor for PostResponseExecutor {
        fn with_next(&self) -> bool {
            true
        }

        async fn execute(&self, _context: &mut DnsContext) -> Result<ExecStep> {
            Ok(ExecStep::Next)
        }

        async fn execute_with_next(
            &self,
            context: &mut DnsContext,
            next: Option<crate::plugin::executor::ExecutorNext>,
        ) -> Result<ExecStep> {
            let step = continue_next!(next, context)?;
            context.set_response(context.request.response(Rcode::NXDomain));
            Ok(step)
        }
    }

    #[tokio::test]
    async fn test_handle_request_refuses_non_query_ingress() {
        let observed = Arc::new(Mutex::new(None));
        let request_handle = make_request_handle(Arc::new(CaptureMetaExecutor {
            observed: observed.clone(),
        }));
        let mut request = make_request(21, "example.com.");
        request.set_message_type(crate::proto::MessageType::Response);

        let result = request_handle
            .handle_request(
                request,
                SocketAddr::from(([127, 0, 0, 1], 5303)),
                RequestMeta {
                    server_name: None,
                    url_path: None,
                },
            )
            .await;

        assert_eq!(result.response.rcode(), Rcode::Refused);
        assert_eq!(result.exit, RequestExit::Controlled);
        assert!(
            observed
                .lock()
                .expect("meta capture lock should not be poisoned")
                .is_none(),
            "pipeline must not run for non-query ingress"
        );
    }

    #[tokio::test]
    async fn test_handle_request_answers_badvers_for_unsupported_edns_version() {
        let observed = Arc::new(Mutex::new(None));
        let request_handle = make_request_handle(Arc::new(CaptureMetaExecutor {
            observed: observed.clone(),
        }));
        let mut request = make_request(22, "example.com.");
        request.set_edns(crate::proto::rdata::Edns::new_with_param(
            1232,
            0,
            1,
            Default::default(),
            Vec::new(),
        ));

        let result = request_handle
            .handle_request(
                request,
                SocketAddr::from(([127, 0, 0, 1], 5303)),
                RequestMeta {
                    server_name: None,
                    url_path: None,
                },
            )
            .await;

        assert_eq!(result.response.rcode(), Rcode::BADVERS);
        assert!(result.response.edns().is_some(), "BADVERS requires an OPT");
        assert!(
            observed
                .lock()
                .expect("meta capture lock should not be poisoned")
                .is_none(),
            "pipeline must not run for unsupported EDNS versions"
        );
    }

    #[tokio::test]
    async fn test_handle_request_with_meta_applies_server_name_and_url_path() {
        let observed = Arc::new(Mutex::new(None));
        let request_handle = make_request_handle(Arc::new(CaptureMetaExecutor {
            observed: observed.clone(),
        }));
        let request = make_request(13, "example.com.");

        let _result = request_handle
            .handle_request(
                request,
                SocketAddr::from(([127, 0, 0, 1], 5303)),
                RequestMeta {
                    server_name: Some(Arc::from("dns.example.test")),
                    url_path: Some(Arc::from("/dns-query")),
                },
            )
            .await;

        assert_eq!(
            observed
                .lock()
                .expect("meta capture lock should not be poisoned")
                .clone(),
            Some(ObservedMeta {
                server_name: Some("dns.example.test".to_string()),
                url_path: Some("/dns-query".to_string()),
            })
        );
    }

    #[tokio::test]
    async fn test_handle_request_supports_with_next_entry_executor() {
        let request_handle = make_request_handle(Arc::new(PostResponseExecutor));
        let request = make_request(21, "example.com.");

        let result = request_handle
            .handle_request(
                request,
                SocketAddr::from(([127, 0, 0, 1], 5303)),
                RequestMeta::default(),
            )
            .await;

        assert_eq!(result.response.rcode(), Rcode::NXDomain);
        assert_eq!(result.exit, RequestExit::Completed);
    }

    #[derive(Debug)]
    struct EcsEchoExecutor {
        seen_ecs: Arc<Mutex<bool>>,
    }

    #[async_trait]
    impl Plugin for EcsEchoExecutor {
        fn tag(&self) -> &str {
            "ecs_echo"
        }

        async fn init(&mut self, _context: &crate::plugin::PluginInitContext<'_>) -> Result<()> {
            Ok(())
        }

        async fn destroy(&self) -> Result<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl Executor for EcsEchoExecutor {
        async fn execute(&self, context: &mut DnsContext) -> Result<ExecStep> {
            let seen_ecs = context
                .request
                .edns()
                .as_ref()
                .is_some_and(|edns| edns.option(EdnsCode::Subnet).is_some());
            *self
                .seen_ecs
                .lock()
                .expect("ecs capture lock should not be poisoned") = seen_ecs;

            // Model an upstream that answers with the ECS scope it used.
            let mut response = context.request.response(Rcode::NoError);
            let mut edns = Edns::new();
            edns.insert(EdnsOption::Subnet(ClientSubnet::new(
                IpAddr::V4(Ipv4Addr::new(192, 0, 2, 0)),
                24,
                24,
            )));
            response.set_edns(edns);
            context.set_response(response);
            Ok(ExecStep::Return)
        }
    }

    #[tokio::test]
    async fn test_handle_request_strips_ecs_when_disabled() {
        let seen_ecs = Arc::new(Mutex::new(false));
        let request_handle = make_request_handle_with_disable_ecs(
            Arc::new(EcsEchoExecutor {
                seen_ecs: seen_ecs.clone(),
            }),
            true,
        );

        let result = request_handle
            .handle_request(
                make_ecs_request(31, "example.com."),
                SocketAddr::from(([127, 0, 0, 1], 5303)),
                RequestMeta::default(),
            )
            .await;

        assert!(
            !*seen_ecs
                .lock()
                .expect("ecs capture lock should not be poisoned"),
            "pipeline must not see the client subnet when disable_ecs is enabled"
        );
        assert_eq!(
            result
                .request
                .edns()
                .as_ref()
                .and_then(|edns| edns.option(EdnsCode::Subnet)),
            None,
            "request handed to the pipeline must not carry a subnet"
        );
        let response_edns = result.response.edns().as_ref().expect("OPT is preserved");
        assert_eq!(
            response_edns.option(EdnsCode::Subnet),
            None,
            "response must not echo a subnet when disable_ecs is enabled"
        );
    }

    #[tokio::test]
    async fn test_handle_request_keeps_ecs_by_default() {
        let seen_ecs = Arc::new(Mutex::new(false));
        let request_handle = make_request_handle(Arc::new(EcsEchoExecutor {
            seen_ecs: seen_ecs.clone(),
        }));

        let result = request_handle
            .handle_request(
                make_ecs_request(32, "example.com."),
                SocketAddr::from(([127, 0, 0, 1], 5303)),
                RequestMeta::default(),
            )
            .await;

        assert!(
            *seen_ecs
                .lock()
                .expect("ecs capture lock should not be poisoned"),
            "the pipeline must keep the client subnet by default"
        );
        assert!(
            result
                .request
                .edns()
                .as_ref()
                .and_then(|edns| edns.option(EdnsCode::Subnet))
                .is_some(),
            "the default configuration must not strip the request subnet"
        );
    }
}
