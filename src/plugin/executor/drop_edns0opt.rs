// SPDX-FileCopyrightText: 2025 Sven Shi
// SPDX-License-Identifier: GPL-3.0-or-later

//! `drop_edns0opt` executor plugin.
//!
//! Strips EDNS0 options from both the request and the final response
//! without changing the rest of the DNS exchange.
//!
//! `mode` controls how `codes` is interpreted:
//! - `blacklist` (default): remove the listed option codes and keep the rest.
//! - `whitelist`: keep only the listed option codes and remove the rest.
//!
//! `skip_request_strip` (default `false`) controls whether the request side is
//! stripped; the response side is always processed. Setting it to `true`
//! leaves the request completely untouched.
//!
//! Runtime behavior:
//! - request phase: unless `skip_request_strip` is enabled, applies the
//!   configured mode to the request OPT before downstream executors run.
//! - continuation response phase: always applies the same mode to the response
//!   OPT after downstream executors complete.
//! - both phases preserve the OPT record itself, even when no options remain,
//!   so EDNS version, flags (including DO), and advertised UDP payload size are
//!   kept. The plugin never removes the OPT record or disables EDNS.
//! - requests or responses where nothing needs to be removed are left
//!   untouched, and no OPT record is created just to strip options.
//! - an empty `codes` set makes the plugin a no-op in `blacklist` mode and
//!   removes every option (while keeping the OPT record) in `whitelist` mode.
//!
//! Difference from `forward_edns0opt`:
//! - `forward_edns0opt` copies selected request options into the final response.
//! - `drop_edns0opt` removes selected options from both the request and the
//!   final response, without changing the rest of the DNS exchange.

use ahash::AHashSet;
use async_trait::async_trait;
use serde::Deserialize;
use serde_yaml_ng::Value;

use crate::config::types::PluginConfig;
use crate::core::context::DnsContext;
use crate::infra::error::{DnsError, Result};
use crate::plugin::executor::{ExecStep, Executor, ExecutorNext};
use crate::plugin::{Plugin, PluginFactory, UninitializedPlugin};
use crate::proto::{Edns, EdnsCode, Message};
use crate::{continue_next, plugin_factory};

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DropEdns0Mode {
    /// Remove the listed option codes and keep the rest.
    #[default]
    Blacklist,
    /// Keep only the listed option codes and remove the rest.
    Whitelist,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct DropEdns0OptConfig {
    /// EDNS option codes interpreted according to `mode`.
    #[serde(default)]
    codes: Vec<u16>,
    /// How `codes` is interpreted; defaults to `blacklist`.
    #[serde(default)]
    mode: DropEdns0Mode,
    /// Skip request-side stripping; defaults to `false`.
    #[serde(default)]
    skip_request_strip: bool,
}

#[derive(Debug)]
struct DropEdns0Opt {
    tag: String,
    mode: DropEdns0Mode,
    code_set: AHashSet<u16>,
    skip_request_strip: bool,
}

#[async_trait]
impl Plugin for DropEdns0Opt {
    fn tag(&self) -> &str {
        &self.tag
    }

    async fn init(&mut self, _context: &crate::plugin::PluginInitContext<'_>) -> Result<()> {
        Ok(())
    }

    async fn destroy(&self) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl Executor for DropEdns0Opt {
    fn with_next(&self) -> bool {
        true
    }

    #[hotpath::measure]
    async fn execute(&self, context: &mut DnsContext) -> Result<ExecStep> {
        self.execute_with_next(context, None).await
    }

    #[hotpath::measure]
    async fn execute_with_next(
        &self,
        context: &mut DnsContext,
        next: Option<ExecutorNext>,
    ) -> Result<ExecStep> {
        if self.mode == DropEdns0Mode::Blacklist && self.code_set.is_empty() {
            return continue_next!(next, context);
        }

        if !self.skip_request_strip {
            self.strip_options(&mut context.request);
        }

        let step = continue_next!(next, context)?;

        if let Some(response) = context.response_mut() {
            self.strip_options(response);
        }

        Ok(step)
    }
}

impl DropEdns0Opt {
    /// Apply the configured mode without touching the rest of the OPT.
    ///
    /// The OPT record is intentionally kept (even when it becomes empty) so its
    /// version, flags, DO bit, and UDP payload size survive unchanged.
    fn strip_options(&self, message: &mut Message) {
        let Some(edns) = message.edns_mut().as_mut() else {
            return;
        };

        match self.mode {
            DropEdns0Mode::Blacklist => {
                for code in &self.code_set {
                    edns.remove(EdnsCode::from(*code));
                }
            }
            DropEdns0Mode::Whitelist => {
                let kept: Vec<_> = edns
                    .options()
                    .iter()
                    .filter(|option| self.code_set.contains(&u16::from(EdnsCode::from(*option))))
                    .cloned()
                    .collect();
                if kept.len() == edns.options().len() {
                    return;
                }
                *edns = Edns::new_with_param(
                    edns.udp_payload_size(),
                    edns.ext_rcode(),
                    edns.version(),
                    *edns.flags(),
                    kept,
                );
            }
        }
    }
}

#[derive(Debug, Clone)]
#[plugin_factory("drop_edns0opt")]
pub struct DropEdns0OptFactory;

impl PluginFactory for DropEdns0OptFactory {
    fn create(
        &self,
        plugin_config: &PluginConfig,
        _init_context: &crate::plugin::PluginInitContext<'_>,
    ) -> Result<UninitializedPlugin> {
        let settings = parse_codes_from_value(plugin_config.args.clone())?;
        Ok(UninitializedPlugin::Executor(Box::new(DropEdns0Opt {
            tag: plugin_config.tag.clone(),
            mode: settings.mode,
            code_set: settings.code_set,
            skip_request_strip: settings.skip_request_strip,
        })))
    }

    fn quick_setup(&self, tag: &str, param: Option<String>) -> Result<UninitializedPlugin> {
        let settings = parse_quick_setup(param.as_deref().unwrap_or_default())?;

        Ok(UninitializedPlugin::Executor(Box::new(DropEdns0Opt {
            tag: tag.to_string(),
            mode: settings.mode,
            code_set: settings.code_set,
            skip_request_strip: settings.skip_request_strip,
        })))
    }
}

#[derive(Debug)]
struct DropEdns0Settings {
    mode: DropEdns0Mode,
    code_set: AHashSet<u16>,
    skip_request_strip: bool,
}

fn parse_codes_from_value(args: Option<Value>) -> Result<DropEdns0Settings> {
    let Some(args) = args else {
        return Ok(DropEdns0Settings {
            mode: DropEdns0Mode::default(),
            code_set: AHashSet::new(),
            skip_request_strip: false,
        });
    };

    if let Some(raw) = args.as_str() {
        return parse_quick_setup(raw);
    }

    let cfg: DropEdns0OptConfig = serde_yaml_ng::from_value(args)
        .map_err(|e| DnsError::plugin(format!("failed to parse drop_edns0opt config: {}", e)))?;

    Ok(DropEdns0Settings {
        mode: cfg.mode,
        code_set: cfg.codes.into_iter().collect(),
        skip_request_strip: cfg.skip_request_strip,
    })
}

fn parse_quick_setup(raw: &str) -> Result<DropEdns0Settings> {
    let mut tokens = split_tokens(raw);
    let mut mode = DropEdns0Mode::default();
    if let Some(parsed) = tokens.first().copied().and_then(parse_mode_token) {
        mode = parsed;
        tokens.remove(0);
    }

    let mut code_set = AHashSet::new();
    for token in tokens {
        let code = token.parse::<u16>().map_err(|e| {
            DnsError::plugin(format!("invalid EDNS0 option code '{}': {}", token, e))
        })?;
        code_set.insert(code);
    }

    Ok(DropEdns0Settings {
        mode,
        code_set,
        skip_request_strip: false,
    })
}

fn parse_mode_token(raw: &str) -> Option<DropEdns0Mode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "blacklist" => Some(DropEdns0Mode::Blacklist),
        "whitelist" => Some(DropEdns0Mode::Whitelist),
        _ => None,
    }
}

fn split_tokens(raw: &str) -> Vec<&str> {
    raw.split(|c: char| c == ',' || c.is_ascii_whitespace())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use super::*;
    use crate::proto::{
        ClientSubnet, DNSClass, Edns, EdnsOption, EdnsPadding, Message, Name, Question, RecordType,
    };

    fn make_request() -> Message {
        let mut request = Message::new();
        request.add_question(Question::new(
            Name::from_ascii("example.com.").unwrap(),
            RecordType::A,
            DNSClass::IN,
        ));
        request
    }

    fn subnet_option() -> EdnsOption {
        EdnsOption::Subnet(ClientSubnet::new(
            IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
            24,
            0,
        ))
    }

    fn padding_option() -> EdnsOption {
        EdnsOption::Padding(EdnsPadding::new(vec![0; 4]))
    }

    fn make_context_with_options(options: Vec<EdnsOption>) -> DnsContext {
        let mut request = make_request();
        let mut edns = Edns::new();
        edns.set_udp_payload_size(1232);
        edns.set_dnssec_ok(true);
        for option in options {
            edns.insert(option);
        }
        request.set_edns(edns);
        DnsContext::new(SocketAddr::from((Ipv4Addr::LOCALHOST, 5300)), request)
    }

    fn make_response_with_options(options: Vec<EdnsOption>) -> Message {
        let mut response = Message::new();
        let mut edns = Edns::new();
        edns.set_udp_payload_size(1400);
        edns.set_dnssec_ok(true);
        for option in options {
            edns.insert(option);
        }
        response.set_edns(edns);
        response
    }

    fn make_plugin(codes: &[u16]) -> DropEdns0Opt {
        make_plugin_with_mode(codes, DropEdns0Mode::Blacklist)
    }

    fn make_plugin_with_mode(codes: &[u16], mode: DropEdns0Mode) -> DropEdns0Opt {
        DropEdns0Opt {
            tag: "strip_opt".to_string(),
            mode,
            code_set: codes.iter().copied().collect(),
            skip_request_strip: false,
        }
    }

    fn make_plugin_with_skip_request_strip(
        codes: &[u16],
        mode: DropEdns0Mode,
        skip_request_strip: bool,
    ) -> DropEdns0Opt {
        DropEdns0Opt {
            tag: "strip_opt".to_string(),
            mode,
            code_set: codes.iter().copied().collect(),
            skip_request_strip,
        }
    }

    #[test]
    fn test_parse_codes_from_value_validation() {
        assert!(parse_codes_from_value(Some(Value::String("x".into()))).is_err());
        assert!(
            parse_codes_from_value(Some(serde_yaml_ng::from_str("codes: [8, 12]").unwrap()))
                .is_ok()
        );
        assert!(parse_codes_from_value(Some(Value::String("8, 12".into()))).is_ok());
    }

    #[test]
    fn test_default_mode_is_blacklist() {
        let settings = parse_codes_from_value(Some(serde_yaml_ng::from_str("codes: [8]").unwrap()))
            .expect("config should parse");
        assert_eq!(settings.mode, DropEdns0Mode::Blacklist);
        assert_eq!(settings.code_set.into_iter().collect::<Vec<_>>(), vec![8]);
        assert!(!settings.skip_request_strip);

        let settings = parse_codes_from_value(None).expect("empty args should parse");
        assert_eq!(settings.mode, DropEdns0Mode::Blacklist);
        assert!(!settings.skip_request_strip);
    }

    #[test]
    fn test_parse_mode_from_config_and_quick_setup() {
        let settings = parse_codes_from_value(Some(
            serde_yaml_ng::from_str("mode: whitelist\ncodes: [8, 12]").unwrap(),
        ))
        .expect("config should parse");
        assert_eq!(settings.mode, DropEdns0Mode::Whitelist);
        assert_eq!(settings.code_set.len(), 2);

        let settings = parse_quick_setup("whitelist 8,12").expect("quick setup should parse");
        assert_eq!(settings.mode, DropEdns0Mode::Whitelist);
        assert!(settings.code_set.contains(&8) && settings.code_set.contains(&12));
        assert!(!settings.skip_request_strip);

        let settings = parse_quick_setup("10,12").expect("quick setup should parse");
        assert_eq!(settings.mode, DropEdns0Mode::Blacklist);
        assert!(!settings.skip_request_strip);
    }

    #[test]
    fn test_skip_request_strip_option_parsing() {
        let settings = parse_codes_from_value(Some(
            serde_yaml_ng::from_str("skip_request_strip: false\ncodes: [8]").unwrap(),
        ))
        .expect("config should parse");
        assert!(!settings.skip_request_strip);

        let settings = parse_codes_from_value(Some(
            serde_yaml_ng::from_str("skip_request_strip: true\ncodes: [8]").unwrap(),
        ))
        .expect("config should parse");
        assert!(settings.skip_request_strip);
    }

    #[tokio::test]
    async fn test_single_code_removes_matching_options_only() {
        let plugin = make_plugin(&[8]);
        let mut ctx = make_context_with_options(vec![subnet_option(), padding_option()]);

        let step = plugin
            .execute(&mut ctx)
            .await
            .expect("execute should succeed");

        assert!(matches!(step, ExecStep::Next));
        let edns = ctx
            .request()
            .edns()
            .as_ref()
            .expect("OPT must be preserved");
        assert_eq!(edns.options(), &[padding_option()]);
        assert_eq!(edns.udp_payload_size(), 1232);
        assert!(edns.flags().dnssec_ok);
        assert_eq!(edns.version(), 0);
    }

    #[tokio::test]
    async fn test_multiple_codes_remove_all_matching_options() {
        let plugin = make_plugin(&[8, 12]);
        let mut ctx =
            make_context_with_options(vec![subnet_option(), padding_option(), subnet_option()]);

        let step = plugin
            .execute(&mut ctx)
            .await
            .expect("execute should succeed");

        assert!(matches!(step, ExecStep::Next));
        let edns = ctx
            .request()
            .edns()
            .as_ref()
            .expect("empty OPT must be preserved");
        assert!(edns.options().is_empty());
        assert_eq!(edns.udp_payload_size(), 1232);
        assert!(edns.flags().dnssec_ok);
    }

    #[tokio::test]
    async fn test_unmatched_options_are_untouched() {
        let plugin = make_plugin(&[10]);
        let mut ctx = make_context_with_options(vec![padding_option(), subnet_option()]);

        let step = plugin
            .execute(&mut ctx)
            .await
            .expect("execute should succeed");

        assert!(matches!(step, ExecStep::Next));
        let edns = ctx.request().edns().as_ref().expect("OPT must stay");
        assert_eq!(edns.options(), &[padding_option(), subnet_option()]);
    }

    #[tokio::test]
    async fn test_empty_codes_is_noop() {
        let plugin = make_plugin(&[]);
        let mut ctx = make_context_with_options(vec![subnet_option(), padding_option()]);

        let step = plugin
            .execute(&mut ctx)
            .await
            .expect("execute should succeed");

        assert!(matches!(step, ExecStep::Next));
        let edns = ctx.request().edns().as_ref().expect("OPT must stay");
        assert_eq!(edns.options(), &[subnet_option(), padding_option()]);
    }

    #[tokio::test]
    async fn test_request_without_edns_is_noop() {
        let plugin = make_plugin(&[8]);
        let mut ctx = DnsContext::new(
            SocketAddr::from((Ipv4Addr::LOCALHOST, 5301)),
            make_request(),
        );

        let step = plugin
            .execute(&mut ctx)
            .await
            .expect("execute should succeed");

        assert!(matches!(step, ExecStep::Next));
        assert!(ctx.request().edns().is_none());
    }

    #[tokio::test]
    async fn test_whitelist_keeps_only_listed_options() {
        let plugin = make_plugin_with_mode(&[8], DropEdns0Mode::Whitelist);
        let mut ctx = make_context_with_options(vec![padding_option(), subnet_option()]);

        let step = plugin
            .execute(&mut ctx)
            .await
            .expect("execute should succeed");

        assert!(matches!(step, ExecStep::Next));
        let edns = ctx.request().edns().as_ref().expect("OPT must stay");
        assert_eq!(edns.options(), &[subnet_option()]);
        assert_eq!(edns.udp_payload_size(), 1232);
        assert!(edns.flags().dnssec_ok);
        assert_eq!(edns.version(), 0);
    }

    #[tokio::test]
    async fn test_whitelist_unmatched_options_are_untouched() {
        let plugin = make_plugin_with_mode(&[8, 12], DropEdns0Mode::Whitelist);
        let mut ctx = make_context_with_options(vec![padding_option(), subnet_option()]);

        let step = plugin
            .execute(&mut ctx)
            .await
            .expect("execute should succeed");

        assert!(matches!(step, ExecStep::Next));
        let edns = ctx.request().edns().as_ref().expect("OPT must stay");
        assert_eq!(edns.options(), &[padding_option(), subnet_option()]);
    }

    #[tokio::test]
    async fn test_whitelist_empty_codes_clears_all_options() {
        let plugin = make_plugin_with_mode(&[], DropEdns0Mode::Whitelist);
        let mut ctx = make_context_with_options(vec![padding_option(), subnet_option()]);

        plugin
            .execute(&mut ctx)
            .await
            .expect("execute should succeed");

        let edns = ctx.request().edns().as_ref().expect("OPT must stay");
        assert!(edns.options().is_empty());
        assert_eq!(edns.udp_payload_size(), 1232);
        assert!(edns.flags().dnssec_ok);
    }

    #[tokio::test]
    async fn test_skip_request_strip_leaves_request_and_strips_response() {
        let plugin = make_plugin_with_skip_request_strip(&[8], DropEdns0Mode::Blacklist, true);
        let mut ctx = make_context_with_options(vec![subnet_option(), padding_option()]);
        ctx.set_response(make_response_with_options(vec![
            subnet_option(),
            padding_option(),
        ]));

        plugin
            .execute(&mut ctx)
            .await
            .expect("execute should succeed");

        let request_edns = ctx
            .request()
            .edns()
            .as_ref()
            .expect("request OPT must stay");
        assert_eq!(
            request_edns.options(),
            &[subnet_option(), padding_option()],
            "request must be untouched when skip_request_strip is enabled"
        );

        let response_edns = ctx
            .response()
            .expect("response should exist")
            .edns()
            .as_ref()
            .expect("response OPT must stay");
        assert_eq!(
            response_edns.options(),
            &[padding_option()],
            "response must still be stripped when skip_request_strip is enabled"
        );
        assert_eq!(response_edns.udp_payload_size(), 1400);
    }

    #[tokio::test]
    async fn test_response_matching_option_is_removed() {
        let plugin = make_plugin(&[8]);
        let mut ctx = make_context_with_options(vec![subnet_option()]);
        ctx.set_response(make_response_with_options(vec![
            subnet_option(),
            padding_option(),
        ]));

        plugin
            .execute(&mut ctx)
            .await
            .expect("execute should succeed");

        let response = ctx.response().expect("response should exist");
        let edns = response
            .edns()
            .as_ref()
            .expect("response OPT must be preserved");
        assert_eq!(edns.options(), &[padding_option()]);
        assert_eq!(edns.udp_payload_size(), 1400);
        assert!(edns.flags().dnssec_ok);
        assert_eq!(edns.version(), 0);
    }

    #[tokio::test]
    async fn test_response_multiple_codes_are_removed() {
        let plugin = make_plugin(&[8, 12]);
        let mut ctx = make_context_with_options(vec![subnet_option()]);
        ctx.set_response(make_response_with_options(vec![
            padding_option(),
            subnet_option(),
        ]));

        plugin
            .execute(&mut ctx)
            .await
            .expect("execute should succeed");

        let response = ctx.response().expect("response should exist");
        let edns = response
            .edns()
            .as_ref()
            .expect("empty response OPT must be preserved");
        assert!(edns.options().is_empty());
        assert_eq!(edns.udp_payload_size(), 1400);
        assert!(edns.flags().dnssec_ok);
    }

    #[tokio::test]
    async fn test_response_without_matching_option_is_untouched() {
        let plugin = make_plugin(&[10]);
        let mut ctx = make_context_with_options(vec![subnet_option()]);
        ctx.set_response(make_response_with_options(vec![
            padding_option(),
            subnet_option(),
        ]));

        plugin
            .execute(&mut ctx)
            .await
            .expect("execute should succeed");

        let response = ctx.response().expect("response should exist");
        let edns = response.edns().as_ref().expect("response OPT must stay");
        assert_eq!(edns.options(), &[padding_option(), subnet_option()]);
        assert_eq!(edns.udp_payload_size(), 1400);
    }

    #[tokio::test]
    async fn test_whitelist_response_keeps_only_listed_options() {
        let plugin = make_plugin_with_mode(&[12], DropEdns0Mode::Whitelist);
        let mut ctx = make_context_with_options(vec![subnet_option()]);
        ctx.set_response(make_response_with_options(vec![
            padding_option(),
            subnet_option(),
        ]));

        plugin
            .execute(&mut ctx)
            .await
            .expect("execute should succeed");

        let response = ctx.response().expect("response should exist");
        let edns = response.edns().as_ref().expect("response OPT must stay");
        assert_eq!(edns.options(), &[padding_option()]);
        assert_eq!(edns.udp_payload_size(), 1400);
        assert!(edns.flags().dnssec_ok);
    }

    #[tokio::test]
    async fn test_whitelist_response_unmatched_options_are_untouched() {
        let plugin = make_plugin_with_mode(&[8, 12], DropEdns0Mode::Whitelist);
        let mut ctx = make_context_with_options(vec![subnet_option()]);
        ctx.set_response(make_response_with_options(vec![
            padding_option(),
            subnet_option(),
        ]));

        plugin
            .execute(&mut ctx)
            .await
            .expect("execute should succeed");

        let response = ctx.response().expect("response should exist");
        let edns = response.edns().as_ref().expect("response OPT must stay");
        assert_eq!(edns.options(), &[padding_option(), subnet_option()]);
        assert_eq!(edns.udp_payload_size(), 1400);
    }

    #[tokio::test]
    async fn test_response_without_edns_does_not_create_opt() {
        let plugin = make_plugin(&[8]);
        let mut ctx = make_context_with_options(vec![subnet_option()]);
        ctx.set_response(Message::new());

        plugin
            .execute(&mut ctx)
            .await
            .expect("execute should succeed");

        assert!(
            ctx.response()
                .expect("response should exist")
                .edns()
                .is_none()
        );
    }
}
