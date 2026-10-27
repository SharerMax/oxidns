// SPDX-FileCopyrightText: 2025 Sven Shi
// SPDX-License-Identifier: GPL-3.0-or-later

//! Application assembly helpers for wiring API and plugin runtime components.

use std::sync::Arc;

#[cfg(feature = "api")]
use crate::api::{
    self, ApiHub, ApiRegister, clear_global_api, global_api_register, install_global_api,
    restore_global_api,
};
use crate::config::types::Config;
use crate::infra::control::AppController;
use crate::infra::error::Result;
use crate::plugin;

#[derive(Debug, Default)]
pub struct AppAssembly {
    #[cfg(feature = "api")]
    pub api_hub: Option<Arc<ApiHub>>,
}

/// A candidate assembly built by [`prepare_assembly`] while the previous
/// generation still serves. Activated into a live [`AppAssembly`] by
/// [`activate_assembly`] once the previous generation has been stopped.
pub struct StagedAssembly {
    #[cfg(feature = "api")]
    pub api_hub: Option<Arc<ApiHub>>,
    pub runtime: Arc<plugin::PluginRuntime>,
    pub config: Config,
}

pub async fn assemble(
    config: &Config,
    controller: Option<Arc<AppController>>,
) -> Result<AppAssembly> {
    let staged = prepare_assembly(config, controller.clone()).await?;
    activate_assembly(staged, controller).await
}

/// Build a candidate assembly without touching process globals beyond the
/// staged metric parking: the candidate plugin runtime initializes (binding
/// its listeners for SO_REUSEPORT overlap on Unix) while the previous
/// generation keeps serving.
///
/// On failure the candidate is destroyed and the caller's live assembly is
/// untouched; metric staging is aborted.
pub async fn prepare_assembly(
    config: &Config,
    controller: Option<Arc<AppController>>,
) -> Result<StagedAssembly> {
    crate::infra::network::metrics::init()?;

    #[cfg(feature = "api")]
    let api_hub = ApiHub::from_config(&config.api)?;
    #[cfg(feature = "api")]
    let previous_api_register = global_api_register();
    #[cfg(feature = "api")]
    if let Some(api_hub) = &api_hub {
        // Plugins register their scoped routes from `init()`. Publish the
        // candidate hub first so those routes land in the hub that will be
        // started on activation instead of the previous generation's hub.
        install_global_api(api_hub.clone());
    } else {
        clear_global_api();
    }
    #[cfg(not(feature = "api"))]
    if config.api.http.is_some() {
        tracing::warn!(
            "api.http is configured but this build was compiled without the `api` feature; \
             the management API (health / control / logs / config endpoints) will not start. \
             Rebuild with --features api to enable it, or remove the api block to silence this warning."
        );
    }

    if let Some(controller) = &controller {
        plugin::set_app_controller(controller.clone());
    } else {
        plugin::clear_app_controller();
    }

    let runtime = match plugin::stage_runtime(config.clone()).await {
        Ok(runtime) => runtime,
        Err(err) => {
            #[cfg(feature = "api")]
            restore_global_api(previous_api_register);
            // The controller only belongs to a caller that had none (fresh
            // startup); a reload shares it with the still-serving assembly.
            if controller.is_none() {
                plugin::clear_app_controller();
            }
            return Err(err);
        }
    };

    Ok(StagedAssembly {
        #[cfg(feature = "api")]
        api_hub,
        runtime,
        config: config.clone(),
    })
}

/// Publish a staged assembly: commit the runtime (outbound profiles + global
/// pointer + staged metrics) and bring the management API up. The previous
/// generation must already be stopped.
pub async fn activate_assembly(
    staged: StagedAssembly,
    controller: Option<Arc<AppController>>,
) -> Result<AppAssembly> {
    let StagedAssembly {
        #[cfg(feature = "api")]
        api_hub,
        runtime,
        config,
    } = staged;

    let runtime = match plugin::commit_staged_runtime(runtime, &config).await {
        Ok(runtime) => runtime,
        Err(err) => {
            plugin::clear_app_controller();
            return Err(err);
        }
    };

    #[cfg(feature = "api")]
    if let Some(api_hub) = &api_hub {
        install_global_api(api_hub.clone());
        let activated = async {
            api::register_builtin_routes()?;
            if let Some(controller) = &controller {
                api::register_control_routes(controller.clone())?;
            }
            let register = ApiRegister::new(api_hub.clone());
            api::register_plugin_runtime_control_routes(&register, &runtime)?;
            api_hub.mark_plugins_initialized(runtime.plugin_count(), runtime.server_plugin_count());
            api_hub.start().await
        };
        if let Err(err) = activated.await {
            plugin::destroy_runtime().await;
            clear_global_api();
            plugin::clear_app_controller();
            return Err(err);
        }
    } else {
        #[cfg(feature = "api")]
        clear_global_api();
    }
    #[cfg(not(feature = "api"))]
    {
        let _ = runtime;
        let _ = controller;
    }

    Ok(AppAssembly {
        #[cfg(feature = "api")]
        api_hub,
    })
}

pub async fn stop(assembly: &AppAssembly) {
    #[cfg(feature = "api")]
    {
        clear_global_api();
        if let Some(api_hub) = &assembly.api_hub {
            api_hub.stop().await;
        }
    }
    #[cfg(not(feature = "api"))]
    let _ = assembly;
    plugin::destroy_runtime().await;
    plugin::clear_app_controller();
}

#[cfg(all(test, feature = "api"))]
mod tests {
    use super::*;
    use crate::api::{
        ApiHub, ApiRegister, global_api_register, global_api_test_guard,
        set_global_api_register_for_test,
    };
    use crate::config::types::{
        ApiConfig, ApiHttpConfig, LogConfig, NetworkConfig, PluginConfig, RuntimeConfig,
    };
    use crate::infra::clock::AppClock;

    #[tokio::test]
    async fn assemble_without_api_config_does_not_register_api() {
        let _guard = global_api_test_guard().await;
        AppClock::start();
        let stale_hub = ApiHub::from_config(&ApiConfig {
            http: Some(ApiHttpConfig::Listen("127.0.0.1:0".to_string())),
        })
        .expect("stale api config should parse")
        .expect("stale api hub should exist");
        set_global_api_register_for_test(Some(ApiRegister::new(stale_hub)));

        let assembly = assemble(
            &Config {
                include: Vec::new(),
                runtime: RuntimeConfig::default(),
                api: ApiConfig::default(),
                log: LogConfig::default(),
                network: NetworkConfig::default(),
                plugins: Vec::new(),
            },
            None,
        )
        .await
        .expect("empty config should assemble");

        assert!(assembly.api_hub.is_none());
        assert!(global_api_register().is_none());

        stop(&assembly).await;
    }

    #[tokio::test]
    async fn prepare_assembly_registers_plugin_scoped_api_routes() {
        let _guard = global_api_test_guard().await;
        AppClock::start();
        clear_global_api();
        plugin::reset_runtime_for_test().await;
        #[cfg(debug_assertions)]
        plugin::enable_runtime_test_serialization();

        let staged = prepare_assembly(
            &Config {
                include: Vec::new(),
                runtime: RuntimeConfig::default(),
                api: ApiConfig {
                    http: Some(ApiHttpConfig::Listen("127.0.0.1:0".to_string())),
                },
                log: LogConfig::default(),
                network: NetworkConfig::default(),
                plugins: vec![PluginConfig {
                    tag: "dns_cache".to_string(),
                    plugin_type: "cache".to_string(),
                    args: None,
                }],
            },
            None,
        )
        .await
        .expect("cache assembly should stage");

        let api_hub = staged.api_hub.as_ref().expect("api hub should exist");
        assert!(
            api_hub.has_route(&http::Method::GET, "/plugins/dns_cache/entries"),
            "plugin-scoped cache route should be registered during plugin init"
        );

        staged.runtime.destroy().await;
        plugin::destroy_runtime().await;
        clear_global_api();
    }

    #[tokio::test]
    async fn staged_reload_registers_plugin_scoped_routes_on_the_candidate_hub() {
        let _guard = global_api_test_guard().await;
        AppClock::start();
        clear_global_api();
        plugin::reset_runtime_for_test().await;
        #[cfg(debug_assertions)]
        plugin::enable_runtime_test_serialization();

        let config = Config {
            include: Vec::new(),
            runtime: RuntimeConfig::default(),
            api: ApiConfig {
                http: Some(ApiHttpConfig::Listen("127.0.0.1:0".to_string())),
            },
            log: LogConfig::default(),
            network: NetworkConfig::default(),
            plugins: vec![PluginConfig {
                tag: "dns_cache".to_string(),
                plugin_type: "cache".to_string(),
                args: None,
            }],
        };

        let old_assembly = assemble(&config, None)
            .await
            .expect("initial assembly should start");
        let old_hub = old_assembly
            .api_hub
            .as_ref()
            .expect("old api hub should exist");

        let staged = prepare_assembly(&config, None)
            .await
            .expect("candidate should stage with the plugin route already registered");
        let candidate_hub = staged
            .api_hub
            .as_ref()
            .expect("candidate api hub should exist");

        assert!(
            old_hub.has_route(&http::Method::GET, "/plugins/dns_cache/entries"),
            "live generation should keep its plugin route while the candidate is staged"
        );
        assert!(
            candidate_hub.has_route(&http::Method::GET, "/plugins/dns_cache/entries"),
            "candidate hub should receive the plugin route during staging"
        );

        stop(&old_assembly).await;
        let new_assembly = activate_assembly(staged, None)
            .await
            .expect("candidate should activate");
        let new_hub = new_assembly
            .api_hub
            .as_ref()
            .expect("new api hub should exist");
        assert!(
            new_hub.has_route(&http::Method::GET, "/plugins/dns_cache/entries"),
            "candidate hub should serve the plugin-scoped route after activation"
        );

        stop(&new_assembly).await;
    }
}

#[cfg(all(test, not(feature = "api")))]
mod tests {
    use super::*;
    use crate::config::types::{ApiConfig, ApiHttpConfig, LogConfig, NetworkConfig, RuntimeConfig};
    use crate::infra::clock::AppClock;

    /// Without the `api` feature, a config that still sets `api.http` is a
    /// version/feature mismatch that does not prevent the server from running:
    /// `assemble` should warn and succeed (no management listener), not error.
    #[tokio::test]
    async fn assemble_warns_but_succeeds_when_api_http_set_without_feature() {
        AppClock::start();
        let assembly = assemble(
            &Config {
                include: Vec::new(),
                runtime: RuntimeConfig::default(),
                api: ApiConfig {
                    http: Some(ApiHttpConfig::Listen("127.0.0.1:0".to_string())),
                },
                log: LogConfig::default(),
                network: NetworkConfig::default(),
                plugins: Vec::new(),
            },
            None,
        )
        .await
        .expect("api.http without the api feature should warn, not fail");

        stop(&assembly).await;
    }
}
