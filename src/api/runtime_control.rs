// SPDX-FileCopyrightText: 2025 Sven Shi
// SPDX-License-Identifier: GPL-3.0-or-later

//! Domain-specific runtime controls exposed through the management API.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use http::{Request, StatusCode};
use serde::{Deserialize, Serialize};

use crate::api::{ApiHandler, ApiRegister, json_error, json_ok};
use crate::infra::error::Result as DnsResult;
use crate::plugin::matcher::MatcherRuntimeMode;
use crate::plugin::provider::{ProviderReloadError, ProviderRuntimeControl};
use crate::plugin::runtime_control::PluginRuntimeControl;
use crate::plugin::{PluginRuntime, current_runtime};

#[derive(Debug, Serialize)]
struct MatcherStatusResponse {
    ok: bool,
    matcher: String,
    mode: MatcherRuntimeMode,
}

#[derive(Debug)]
struct MatcherStatusHandler {
    tag: String,
}

#[derive(Debug, Deserialize)]
struct MatcherModeRequest {
    mode: MatcherRuntimeMode,
}

#[derive(Debug)]
struct MatcherModeHandler {
    tag: String,
}

fn live_runtime_control(tag: &str) -> Option<PluginRuntimeControl> {
    current_runtime()?.get_plugin(tag)?.runtime_control()
}

fn runtime_control_unavailable(tag: &str, kind: &str) -> crate::api::ApiResponse {
    json_error(
        StatusCode::NOT_FOUND,
        "plugin_runtime_control_unavailable",
        format!("{} runtime control '{}' is not available", kind, tag),
    )
}

#[async_trait]
impl ApiHandler for MatcherStatusHandler {
    async fn handle(&self, _request: Request<Bytes>) -> crate::api::ApiResponse {
        let Some(PluginRuntimeControl::Matcher(control)) = live_runtime_control(&self.tag) else {
            return runtime_control_unavailable(&self.tag, "matcher");
        };
        json_ok(
            StatusCode::OK,
            &MatcherStatusResponse {
                ok: true,
                matcher: self.tag.clone(),
                mode: control.mode(),
            },
        )
    }
}

#[async_trait]
impl ApiHandler for MatcherModeHandler {
    async fn handle(&self, request: Request<Bytes>) -> crate::api::ApiResponse {
        let desired = match serde_json::from_slice::<MatcherModeRequest>(request.body()) {
            Ok(request) => request.mode,
            Err(err) => {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_matcher_runtime_mode",
                    format!(
                        "request body must contain mode normal, always_false, or always_true: {err}"
                    ),
                );
            }
        };
        let Some(PluginRuntimeControl::Matcher(control)) = live_runtime_control(&self.tag) else {
            return runtime_control_unavailable(&self.tag, "matcher");
        };
        control.set_mode(desired);
        tracing::info!(
            matcher = %self.tag,
            mode = ?desired,
            "matcher runtime control updated"
        );
        json_ok(
            StatusCode::OK,
            &MatcherStatusResponse {
                ok: true,
                matcher: self.tag.clone(),
                mode: control.mode(),
            },
        )
    }
}

#[derive(Debug, Serialize)]
struct ProviderReloadResponse {
    ok: bool,
    action: &'static str,
    provider: String,
    status: &'static str,
    degraded_files: Vec<String>,
}

#[derive(Debug)]
struct ProviderReloadHandler {
    tag: String,
}

#[async_trait]
impl ApiHandler for ProviderReloadHandler {
    async fn handle(&self, _request: Request<Bytes>) -> crate::api::ApiResponse {
        let Some(PluginRuntimeControl::Provider(control)) = live_runtime_control(&self.tag) else {
            return runtime_control_unavailable(&self.tag, "provider");
        };
        reload_provider_response(&self.tag, &control).await
    }
}

async fn reload_provider_response(
    tag: &str,
    control: &ProviderRuntimeControl,
) -> crate::api::ApiResponse {
    match control.reload().await {
        Ok(()) => {
            let degraded_files = control.degraded_files();
            let status = if degraded_files.is_empty() {
                "reloaded"
            } else {
                "degraded"
            };
            json_ok(
                StatusCode::OK,
                &ProviderReloadResponse {
                    ok: true,
                    action: "reload_provider",
                    provider: tag.to_string(),
                    status,
                    degraded_files,
                },
            )
        }
        Err(error @ ProviderReloadError::Busy { .. }) => json_error(
            StatusCode::CONFLICT,
            "provider_reload_busy",
            error.to_string(),
        ),
        Err(ProviderReloadError::Failed(error)) => json_error(
            StatusCode::BAD_REQUEST,
            "provider_reload_failed",
            error.to_string(),
        ),
    }
}

#[derive(Debug, Serialize)]
struct ProviderStatusResponse {
    ok: bool,
    provider: String,
    status: &'static str,
    degraded_files: Vec<String>,
}

#[derive(Debug)]
struct ProviderStatusHandler {
    tag: String,
}

#[async_trait]
impl ApiHandler for ProviderStatusHandler {
    async fn handle(&self, _request: Request<Bytes>) -> crate::api::ApiResponse {
        let Some(PluginRuntimeControl::Provider(control)) = live_runtime_control(&self.tag) else {
            return runtime_control_unavailable(&self.tag, "provider");
        };
        provider_status_response(&self.tag, &control)
    }
}

fn provider_status_response(
    tag: &str,
    control: &ProviderRuntimeControl,
) -> crate::api::ApiResponse {
    let degraded_files = control.degraded_files();
    let status = if degraded_files.is_empty() {
        "running"
    } else {
        "degraded"
    };
    json_ok(
        StatusCode::OK,
        &ProviderStatusResponse {
            ok: true,
            provider: tag.to_string(),
            status,
            degraded_files,
        },
    )
}

pub(crate) fn register_plugin_runtime_control_routes(
    register: &ApiRegister,
    runtime: &PluginRuntime,
) -> DnsResult<()> {
    for (tag, control) in runtime.runtime_controls() {
        let plugin = register.plugin(&tag)?;
        match control {
            PluginRuntimeControl::Matcher(_) => {
                plugin.get(
                    "/status",
                    Arc::new(MatcherStatusHandler { tag: tag.clone() }),
                )?;
                plugin.post("/mode", Arc::new(MatcherModeHandler { tag }))?;
            }
            PluginRuntimeControl::Provider(_) => {
                plugin.get(
                    "/status",
                    Arc::new(ProviderStatusHandler { tag: tag.clone() }),
                )?;
                plugin.post("/reload", Arc::new(ProviderReloadHandler { tag }))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::any::Any;
    use std::sync::atomic::{AtomicBool, Ordering};

    use http_body_util::BodyExt;
    use tokio::sync::Notify;

    use super::*;
    use crate::plugin::Plugin;
    use crate::plugin::provider::Provider;

    #[derive(Debug)]
    struct BlockingProvider {
        started: Notify,
        release: Notify,
    }

    #[async_trait]
    impl Plugin for BlockingProvider {
        fn tag(&self) -> &str {
            "blocking"
        }
    }

    #[async_trait]
    impl Provider for BlockingProvider {
        fn as_any(&self) -> &dyn Any {
            self
        }

        async fn reload(&self) -> DnsResult<()> {
            self.started.notify_one();
            self.release.notified().await;
            Ok(())
        }
    }

    #[derive(Debug)]
    struct DegradableProvider {
        degraded: AtomicBool,
    }

    #[async_trait]
    impl Plugin for DegradableProvider {
        fn tag(&self) -> &str {
            "degradable"
        }
    }

    #[async_trait]
    impl Provider for DegradableProvider {
        fn as_any(&self) -> &dyn Any {
            self
        }

        async fn reload(&self) -> DnsResult<()> {
            Ok(())
        }

        fn degraded_files(&self) -> Vec<String> {
            if self.degraded.load(Ordering::Acquire) {
                vec!["./rules/missing.txt".to_string()]
            } else {
                Vec::new()
            }
        }
    }

    async fn response_json(response: crate::api::ApiResponse) -> serde_json::Value {
        let body = response
            .into_body()
            .collect()
            .await
            .expect("response body should collect")
            .to_bytes();
        serde_json::from_slice(&body).expect("response should be valid json")
    }

    #[tokio::test]
    async fn provider_handler_maps_concurrent_reload_to_conflict() {
        let provider = Arc::new(BlockingProvider {
            started: Notify::new(),
            release: Notify::new(),
        });
        let control = Arc::new(ProviderRuntimeControl::new(provider.clone()));
        let first_control = control.clone();
        let first = tokio::spawn(async move { first_control.reload().await });
        provider.started.notified().await;

        let response = reload_provider_response("blocking", &control).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let payload = response_json(response).await;
        assert_eq!(payload["code"], "provider_reload_busy");

        provider.release.notify_one();
        first
            .await
            .expect("first reload task should finish")
            .unwrap();
    }

    #[tokio::test]
    async fn reload_response_reports_degraded_files() {
        let provider = Arc::new(DegradableProvider {
            degraded: AtomicBool::new(false),
        });
        let control = ProviderRuntimeControl::new(provider.clone());

        let response = reload_provider_response("degradable", &control).await;
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["ok"], true);
        assert_eq!(payload["action"], "reload_provider");
        assert_eq!(payload["status"], "reloaded");
        assert_eq!(payload["degraded_files"], serde_json::json!([]));

        provider.degraded.store(true, Ordering::Release);
        let response = reload_provider_response("degradable", &control).await;
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["ok"], true);
        assert_eq!(payload["status"], "degraded");
        assert_eq!(
            payload["degraded_files"],
            serde_json::json!(["./rules/missing.txt"])
        );
    }

    #[tokio::test]
    async fn provider_status_reports_degraded_state() {
        let provider = Arc::new(DegradableProvider {
            degraded: AtomicBool::new(false),
        });
        let control = ProviderRuntimeControl::new(provider.clone());

        let response = provider_status_response("degradable", &control);
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["ok"], true);
        assert_eq!(payload["provider"], "degradable");
        assert_eq!(payload["status"], "running");
        assert_eq!(payload["degraded_files"], serde_json::json!([]));

        provider.degraded.store(true, Ordering::Release);
        let response = provider_status_response("degradable", &control);
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["status"], "degraded");
        assert_eq!(
            payload["degraded_files"],
            serde_json::json!(["./rules/missing.txt"])
        );
    }
}
