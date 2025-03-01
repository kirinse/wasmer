use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde::Serialize;

pub mod graphql {

    use graphql_client::*;
    #[cfg(not(target_os = "wasi"))]
    use reqwest::{
        blocking::{multipart::Form, Client},
        header::USER_AGENT,
    };
    use std::env;
    use std::time::Duration;
    #[cfg(target_os = "wasi")]
    use {wasm_bus_reqwest::prelude::header::*, wasm_bus_reqwest::prelude::*};

    mod proxy {
        //! Code for dealing with setting things up to proxy network requests
        use thiserror::Error;

        #[derive(Debug, Error)]
        pub enum ProxyError {
            #[error("Failed to parse URL from {}: {}", url_location, error_message)]
            UrlParseError {
                url_location: String,
                error_message: String,
            },

            #[error("Could not connect to proxy: {0}")]
            ConnectionError(String),
        }

        /// Tries to set up a proxy
        ///
        /// This function reads from wapm config's `proxy.url` first, then checks
        /// `ALL_PROXY`, `HTTPS_PROXY`, and `HTTP_PROXY` environment variables, in both
        /// upper case and lower case, in that order.
        ///
        /// If a proxy is specified in wapm config's `proxy.url`, it is assumed
        /// to be a general proxy
        ///
        /// A return value of `Ok(None)` means that there was no attempt to set up a proxy,
        /// `Ok(Some(proxy))` means that the proxy was set up successfully, and `Err(e)` that
        /// there was a failure while attempting to set up the proxy.
        pub fn maybe_set_up_proxy() -> anyhow::Result<Option<reqwest::Proxy>> {
            use std::env;
            let proxy = if let Ok(proxy_url) =
                env::var("ALL_PROXY").or_else(|_| env::var("all_proxy"))
            {
                reqwest::Proxy::all(&proxy_url).map(|proxy| (proxy_url, proxy, "ALL_PROXY"))
            } else if let Ok(https_proxy_url) =
                env::var("HTTPS_PROXY").or_else(|_| env::var("https_proxy"))
            {
                reqwest::Proxy::https(&https_proxy_url)
                    .map(|proxy| (https_proxy_url, proxy, "HTTPS_PROXY"))
            } else if let Ok(http_proxy_url) =
                env::var("HTTP_PROXY").or_else(|_| env::var("http_proxy"))
            {
                reqwest::Proxy::http(&http_proxy_url)
                    .map(|proxy| (http_proxy_url, proxy, "http_proxy"))
            } else {
                return Ok(None);
            }
            .map_err(|e| ProxyError::ConnectionError(e.to_string()))
            .and_then(
                |(proxy_url_str, proxy, url_location): (String, _, &'static str)| {
                    url::Url::parse(&proxy_url_str)
                        .map_err(|e| ProxyError::UrlParseError {
                            url_location: url_location.to_string(),
                            error_message: e.to_string(),
                        })
                        .map(|url| {
                            if !(url.username().is_empty()) && url.password().is_some() {
                                proxy.basic_auth(url.username(), url.password().unwrap_or_default())
                            } else {
                                proxy
                            }
                        })
                },
            )?;

            Ok(Some(proxy))
        }
    }

    #[derive(GraphQLQuery)]
    #[graphql(
        schema_path = "graphql/schema.graphql",
        query_path = "graphql/queries/get_package_version.graphql",
        response_derives = "Debug"
    )]
    pub(crate) struct GetPackageVersionQuery;

    #[derive(GraphQLQuery)]
    #[graphql(
        schema_path = "graphql/schema.graphql",
        query_path = "graphql/queries/get_package_by_command.graphql",
        response_derives = "Debug"
    )]
    pub(crate) struct GetPackageByCommandQuery;

    #[derive(GraphQLQuery)]
    #[graphql(
        schema_path = "graphql/schema.graphql",
        query_path = "graphql/queries/get_packages.graphql",
        response_derives = "Debug"
    )]
    pub(crate) struct GetPackagesQuery;

    #[derive(GraphQLQuery)]
    #[graphql(
        schema_path = "graphql/schema.graphql",
        query_path = "graphql/queries/test_if_registry_present.graphql",
        response_derives = "Debug"
    )]
    pub(crate) struct TestIfRegistryPresent;

    #[cfg(target_os = "wasi")]
    pub fn whoami_distro() -> String {
        whoami::os().to_lowercase()
    }

    #[cfg(not(target_os = "wasi"))]
    pub fn whoami_distro() -> String {
        whoami::distro().to_lowercase()
    }

    pub fn execute_query_modifier_inner_check_json<V, F>(
        registry_url: &str,
        login_token: &str,
        query: &QueryBody<V>,
        timeout: Option<Duration>,
        form_modifier: F,
    ) -> anyhow::Result<()>
    where
        V: serde::Serialize,
        F: FnOnce(Form) -> Form,
    {
        let client = {
            let builder = Client::builder();

            #[cfg(not(target_os = "wasi"))]
            let builder = if let Some(proxy) = proxy::maybe_set_up_proxy()? {
                builder.proxy(proxy)
            } else {
                builder
            };
            builder.build()?
        };

        let vars = serde_json::to_string(&query.variables).unwrap();

        let form = Form::new()
            .text("query", query.query.to_string())
            .text("operationName", query.operation_name.to_string())
            .text("variables", vars);

        let form = form_modifier(form);

        let user_agent = format!(
            "wapm/{} {} {}",
            env!("CARGO_PKG_VERSION"),
            whoami::platform(),
            whoami_distro(),
        );

        let mut res = client
            .post(registry_url)
            .multipart(form)
            .bearer_auth(
                env::var("WAPM_REGISTRY_TOKEN").unwrap_or_else(|_| login_token.to_string()),
            )
            .header(USER_AGENT, user_agent);

        if let Some(t) = timeout {
            res = res.timeout(t);
        }

        let res = res.send()?;

        let _: Response<serde_json::Value> = res.json()?;

        Ok(())
    }

    pub fn execute_query_modifier_inner<R, V, F>(
        registry_url: &str,
        login_token: &str,
        query: &QueryBody<V>,
        timeout: Option<Duration>,
        form_modifier: F,
    ) -> anyhow::Result<R>
    where
        for<'de> R: serde::Deserialize<'de>,
        V: serde::Serialize,
        F: FnOnce(Form) -> Form,
    {
        let client = {
            let builder = Client::builder();

            #[cfg(not(target_os = "wasi"))]
            let builder = if let Some(proxy) = proxy::maybe_set_up_proxy()? {
                builder.proxy(proxy)
            } else {
                builder
            };
            builder.build()?
        };

        let vars = serde_json::to_string(&query.variables).unwrap();

        let form = Form::new()
            .text("query", query.query.to_string())
            .text("operationName", query.operation_name.to_string())
            .text("variables", vars);

        let form = form_modifier(form);

        let user_agent = format!(
            "wapm/{} {} {}",
            env!("CARGO_PKG_VERSION"),
            whoami::platform(),
            whoami_distro(),
        );

        let mut res = client
            .post(registry_url)
            .multipart(form)
            .bearer_auth(
                env::var("WAPM_REGISTRY_TOKEN").unwrap_or_else(|_| login_token.to_string()),
            )
            .header(USER_AGENT, user_agent);

        if let Some(t) = timeout {
            res = res.timeout(t);
        }

        let res = res.send()?;
        let response_body: Response<R> = res.json()?;
        if let Some(errors) = response_body.errors {
            let error_messages: Vec<String> = errors.into_iter().map(|err| err.message).collect();
            return Err(anyhow::anyhow!("{}", error_messages.join(", ")));
        }
        Ok(response_body.data.expect("missing response data"))
    }

    pub fn execute_query<R, V>(
        registry_url: &str,
        login_token: &str,
        query: &QueryBody<V>,
    ) -> anyhow::Result<R>
    where
        for<'de> R: serde::Deserialize<'de>,
        V: serde::Serialize,
    {
        execute_query_modifier_inner(registry_url, login_token, query, None, |f| f)
    }

    pub fn execute_query_with_timeout<R, V>(
        registry_url: &str,
        login_token: &str,
        timeout: Duration,
        query: &QueryBody<V>,
    ) -> anyhow::Result<R>
    where
        for<'de> R: serde::Deserialize<'de>,
        V: serde::Serialize,
    {
        execute_query_modifier_inner(registry_url, login_token, query, Some(timeout), |f| f)
    }
}

pub static GLOBAL_CONFIG_FILE_NAME: &str = if cfg!(target_os = "wasi") {
    "/.private/wapm.toml"
} else {
    "wapm.toml"
};

#[derive(Deserialize, Default, Serialize, Debug, PartialEq)]
pub struct PartialWapmConfig {
    /// The number of seconds to wait before checking the registry for a new
    /// version of the package.
    #[serde(default = "wax_default_cooldown")]
    pub wax_cooldown: i32,

    /// The registry that wapm will connect to.
    pub registry: Registries,

    /// Whether or not telemetry is enabled.
    #[cfg(feature = "telemetry")]
    #[serde(default)]
    pub telemetry: Telemetry,

    /// Whether or not updated notifications are enabled.
    #[cfg(feature = "update-notifications")]
    #[serde(default)]
    pub update_notifications: UpdateNotifications,

    /// The proxy to use when connecting to the Internet.
    #[serde(default)]
    pub proxy: Proxy,
}

pub const fn wax_default_cooldown() -> i32 {
    5 * 60
}

#[derive(Deserialize, Serialize, Debug, PartialEq, Default)]
pub struct Proxy {
    pub url: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, PartialEq, Default)]
pub struct UpdateNotifications {
    pub enabled: String,
}

#[cfg(feature = "telemetry")]
#[derive(Deserialize, Serialize, Debug, PartialEq)]
pub struct Telemetry {
    pub enabled: String,
}

#[derive(Deserialize, Serialize, Debug, PartialEq, Clone)]
#[serde(untagged)]
pub enum Registries {
    Single(Registry),
    Multi(MultiRegistry),
}

#[derive(Deserialize, Serialize, Debug, PartialEq, Clone)]
pub struct MultiRegistry {
    /// Currently active registry
    pub current: String,
    /// Map from "RegistryUrl" to "LoginToken", in order to
    /// be able to be able to easily switch between registries
    pub tokens: BTreeMap<String, String>,
}

impl Default for Registries {
    fn default() -> Self {
        Registries::Single(Registry {
            url: format_graphql("https://registry.wapm.io"),
            token: None,
        })
    }
}

#[derive(Deserialize, Serialize, Debug, PartialEq, Clone)]
pub struct Registry {
    pub url: String,
    pub token: Option<String>,
}

fn format_graphql(registry: &str) -> String {
    if registry.ends_with("/graphql") {
        registry.to_string()
    } else if registry.ends_with('/') {
        format!("{}graphql", registry)
    } else {
        format!("{}/graphql", registry)
    }
}

impl PartialWapmConfig {
    pub fn from_file() -> Result<Self, String> {
        let path = Self::get_file_location()?;

        match std::fs::read_to_string(&path) {
            Ok(config_toml) => {
                toml::from_str(&config_toml).map_err(|e| format!("could not parse {path:?}: {e}"))
            }
            Err(_e) => Ok(Self::default()),
        }
    }

    pub fn get_current_dir() -> std::io::Result<PathBuf> {
        #[cfg(target_os = "wasi")]
        if let Some(pwd) = std::env::var("PWD").ok() {
            return Ok(PathBuf::from(pwd));
        }
        std::env::current_dir()
    }

    pub fn get_folder() -> Result<PathBuf, String> {
        Ok(
            if let Some(folder_str) = env::var("WASMER_DIR").ok().filter(|s| !s.is_empty()) {
                let folder = PathBuf::from(folder_str);
                std::fs::create_dir_all(folder.clone())
                    .map_err(|e| format!("cannot create config directory: {e}"))?;
                folder
            } else {
                #[allow(unused_variables)]
                let default_dir = Self::get_current_dir()
                    .ok()
                    .unwrap_or_else(|| PathBuf::from("/".to_string()));
                #[cfg(feature = "dirs")]
                let home_dir =
                    dirs::home_dir().ok_or(GlobalConfigError::CannotFindHomeDirectory)?;
                #[cfg(not(feature = "dirs"))]
                let home_dir = std::env::var("HOME")
                    .ok()
                    .unwrap_or_else(|| default_dir.to_string_lossy().to_string());
                let mut folder = PathBuf::from(home_dir);
                folder.push(".wasmer");
                std::fs::create_dir_all(folder.clone())
                    .map_err(|e| format!("cannot create config directory: {e}"))?;
                folder
            },
        )
    }

    fn get_file_location() -> Result<PathBuf, String> {
        Ok(Self::get_folder()?.join(GLOBAL_CONFIG_FILE_NAME))
    }
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Eq, Ord)]
pub struct PackageDownloadInfo {
    pub registry: String,
    pub package: String,
    pub version: String,
    pub is_latest_version: bool,
    pub commands: String,
    pub manifest: String,
    pub url: String,
}

pub fn get_command_local(_name: &str) -> Result<PathBuf, String> {
    Err("unimplemented".to_string())
}

pub fn get_package_local_dir(
    registry_host: &str,
    name: &str,
    version: &str,
) -> Result<PathBuf, String> {
    if !name.contains('/') {
        return Err(format!(
            "package name has to be in the format namespace/package: {name:?}"
        ));
    }
    let (namespace, name) = name
        .split_once('/')
        .ok_or_else(|| format!("missing namespace / name for {name:?}"))?;
    let install_dir = get_global_install_dir(registry_host)
        .ok_or_else(|| format!("no install dir for {name:?}"))?;
    Ok(install_dir.join(namespace).join(name).join(version))
}

#[derive(Debug, Clone)]
pub struct LocalPackage {
    pub registry: String,
    pub name: String,
    pub version: String,
}

impl LocalPackage {
    pub fn get_path(&self) -> Result<PathBuf, String> {
        let host = url::Url::parse(&self.registry)
            .ok()
            .and_then(|o| o.host_str().map(|s| s.to_string()))
            .unwrap_or_else(|| self.registry.clone());

        get_package_local_dir(&host, &self.name, &self.version)
    }
}

/// Returns the (manifest, .wasm file name), given a package dir
pub fn get_executable_file_from_path(
    package_dir: &PathBuf,
    command: Option<&str>,
) -> Result<(wapm_toml::Manifest, PathBuf), anyhow::Error> {
    let wapm_toml = std::fs::read_to_string(package_dir.join("wapm.toml"))
        .map_err(|_| anyhow::anyhow!("Package {package_dir:?} has no wapm.toml"))?;

    let wapm_toml = toml::from_str::<wapm_toml::Manifest>(&wapm_toml)
        .map_err(|e| anyhow::anyhow!("Could not parse toml for {package_dir:?}: {e}"))?;

    let name = wapm_toml.package.name.clone();
    let version = wapm_toml.package.version.clone();

    let commands = wapm_toml.command.clone().unwrap_or_default();
    let entrypoint_module = match command {
        Some(s) => commands.iter().find(|c| c.get_name() == s).ok_or_else(|| {
            anyhow::anyhow!("Cannot run {name}@{version}: package has no command {s:?}")
        })?,
        None => commands.first().ok_or_else(|| {
            anyhow::anyhow!("Cannot run {name}@{version}: package has no commands")
        })?,
    };

    let module_name = entrypoint_module.get_module();
    let modules = wapm_toml.module.clone().unwrap_or_default();
    let entrypoint_module = modules
        .iter()
        .find(|m| m.name == module_name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Cannot run {name}@{version}: module {module_name} not found in wapm.toml"
            )
        })?;

    let entrypoint_source = package_dir.join(&entrypoint_module.source);

    Ok((wapm_toml, entrypoint_source))
}

fn get_all_names_in_dir(dir: &PathBuf) -> Vec<(PathBuf, String)> {
    if !dir.is_dir() {
        return Vec::new();
    }

    let read_dir = match std::fs::read_dir(dir) {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };

    let entries = read_dir
        .map(|res| res.map(|e| e.path()))
        .collect::<Result<Vec<_>, std::io::Error>>();

    let registry_entries = match entries {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };

    registry_entries
        .into_iter()
        .filter_map(|re| Some((re.clone(), re.file_name()?.to_str()?.to_string())))
        .collect()
}

/// Returns a list of all locally installed packages
pub fn get_all_local_packages(registry: Option<&str>) -> Vec<LocalPackage> {
    let mut packages = Vec::new();
    let registries = match registry {
        Some(s) => vec![s.to_string()],
        None => get_all_available_registries().unwrap_or_default(),
    };

    let mut registry_hosts = registries
        .into_iter()
        .filter_map(|s| url::Url::parse(&s).ok()?.host_str().map(|s| s.to_string()))
        .collect::<Vec<_>>();

    let mut registries_in_root_dir = get_checkouts_dir()
        .as_ref()
        .map(get_all_names_in_dir)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(path, p)| if path.is_dir() { Some(p) } else { None })
        .collect();

    registry_hosts.append(&mut registries_in_root_dir);
    registry_hosts.sort();
    registry_hosts.dedup();

    for host in registry_hosts {
        let root_dir = match get_global_install_dir(&host) {
            Some(o) => o,
            None => continue,
        };

        for (username_path, user_name) in get_all_names_in_dir(&root_dir) {
            for (package_path, package_name) in get_all_names_in_dir(&username_path) {
                for (version_path, package_version) in get_all_names_in_dir(&package_path) {
                    let _ = match std::fs::read_to_string(version_path.join("wapm.toml")) {
                        Ok(o) => o,
                        Err(_) => continue,
                    };
                    packages.push(LocalPackage {
                        registry: host.clone(),
                        name: format!("{user_name}/{package_name}"),
                        version: package_version,
                    });
                }
            }
        }
    }

    packages
}

pub fn get_local_package(
    registry: Option<&str>,
    name: &str,
    version: Option<&str>,
) -> Option<LocalPackage> {
    get_all_local_packages(registry)
        .iter()
        .find(|p| {
            if p.name != name {
                return false;
            }
            if let Some(v) = version {
                if p.version != v {
                    return false;
                }
            }
            true
        })
        .cloned()
}

pub fn query_command_from_registry(
    registry_url: &str,
    command_name: &str,
) -> Result<PackageDownloadInfo, String> {
    use crate::graphql::{execute_query, get_package_by_command_query, GetPackageByCommandQuery};
    use graphql_client::GraphQLQuery;

    let q = GetPackageByCommandQuery::build_query(get_package_by_command_query::Variables {
        command_name: command_name.to_string(),
    });

    let response: get_package_by_command_query::ResponseData = execute_query(registry_url, "", &q)
        .map_err(|e| format!("Error sending GetPackageByCommandQuery:  {e}"))?;

    let command = response
        .get_command
        .ok_or_else(|| "GetPackageByCommandQuery: no get_command".to_string())?;

    let package = command.package_version.package.display_name;
    let version = command.package_version.version;
    let url = command.package_version.distribution.download_url;

    Ok(PackageDownloadInfo {
        registry: registry_url.to_string(),
        package,
        version,
        is_latest_version: command.package_version.is_last_version,
        manifest: command.package_version.manifest,
        commands: command_name.to_string(),
        url,
    })
}

#[derive(Debug, Clone, PartialEq, PartialOrd)]
pub enum QueryPackageError {
    AmbigouusName {
        name: String,
        packages: Vec<PackageDownloadInfo>,
    },
    ErrorSendingQuery(String),
    NoPackageFound {
        name: String,
        version: Option<String>,
        packages: Vec<PackageDownloadInfo>,
    },
}

impl QueryPackageError {
    pub fn get_packages(&self) -> Vec<PackageDownloadInfo> {
        match self {
            QueryPackageError::AmbigouusName { name: _, packages }
            | QueryPackageError::NoPackageFound {
                name: _,
                version: _,
                packages,
            } => packages.clone(),
            _ => Vec::new(),
        }
    }
}

/// Returns true if a package has a newer version
///
/// Also returns true if the package is not installed yet.
pub fn get_if_package_has_new_version(
    registry_url: &str,
    name: &str,
    version: Option<String>,
    max_timeout: Duration,
) -> Result<Option<PackageDownloadInfo>, String> {
    if !test_if_registry_present(registry_url).unwrap_or(false) {
        return Ok(None);
    }

    let host = match url::Url::parse(registry_url) {
        Ok(o) => match o.host_str().map(|s| s.to_string()) {
            Some(s) => s,
            None => return Err(format!("invalid host: {registry_url}")),
        },
        Err(_) => return Err(format!("invalid host: {registry_url}")),
    };

    let (namespace, name) = name
        .split_once('/')
        .ok_or_else(|| format!("missing namespace / name for {name:?}"))?;

    let package_dir = get_global_install_dir(&host).map(|path| path.join(namespace).join(name));

    let package_dir = match package_dir {
        Some(s) => s,
        None => return Err(format!("package dir does not exist {name}/{namespace}")),
    };

    // all installed versions of this package
    let mut all_installed_versions = Vec::new();

    let may_have_newer_version = match version {
        Some(ref o) => Some(())
            .map(|_| {
                let modified = package_dir.join(&o).metadata().ok()?.modified().ok()?;
                let version = semver::Version::parse(o).ok()?;
                all_installed_versions.push(version);
                if modified.elapsed().ok()? <= max_timeout {
                    Some(())
                } else {
                    None
                }
            })
            .is_some(),
        None => {
            let read_dir = match std::fs::read_dir(&package_dir) {
                Ok(o) => o,
                Err(_) => return Err(format!("{}", package_dir.display())),
            };

            // if there is any package younger than $max_timeout, return false
            read_dir
                .filter_map(|entry| {
                    let entry = entry.ok()?;
                    let version = semver::Version::parse(entry.file_name().to_str()?).ok()?;
                    all_installed_versions.push(version);
                    let modified = entry.metadata().ok()?.modified().ok()?;
                    if modified.elapsed().ok()? <= max_timeout {
                        Some(())
                    } else {
                        None
                    }
                })
                .next()
                .is_some()
        }
    };

    // If the version is specified, don't check for updates
    // (since the package returned from the server would be the same)
    if all_installed_versions.is_empty() || (may_have_newer_version && version.is_none()) {
        let available_packages =
            query_available_packages_from_registry(registry_url, name).unwrap_or_default();

        for p in available_packages {
            if p.is_latest_version {
                return Ok(Some(p));
            }
        }
    }

    Err(format!(
        "Cannot find package {name:?} either locally or in registry"
    ))
}

pub fn query_available_packages_from_registry(
    registry_url: &str,
    name: &str,
) -> Result<Vec<PackageDownloadInfo>, QueryPackageError> {
    use crate::graphql::{execute_query, get_packages_query, GetPackagesQuery};
    use graphql_client::GraphQLQuery;

    let q = GetPackagesQuery::build_query(get_packages_query::Variables {
        names: vec![name.to_string()],
    });

    let response: get_packages_query::ResponseData =
        execute_query(registry_url, "", &q).map_err(|e| {
            QueryPackageError::ErrorSendingQuery(format!("Error sending GetPackagesQuery: {e}"))
        })?;

    let available_packages = response
        .package
        .iter()
        .filter_map(|p| {
            let p = p.as_ref()?;
            let mut versions = Vec::new();

            for v in p.versions.iter() {
                for v in v.iter() {
                    let v = match v.as_ref() {
                        Some(s) => s,
                        None => continue,
                    };

                    let manifest = toml::from_str::<wapm_toml::Manifest>(&v.manifest).ok()?;

                    versions.push(PackageDownloadInfo {
                        registry: registry_url.to_string(),
                        package: p.name.clone(),

                        version: v.version.clone(),
                        is_latest_version: v.is_last_version,
                        manifest: v.manifest.clone(),

                        commands: manifest
                            .command
                            .unwrap_or_default()
                            .iter()
                            .map(|s| s.get_name())
                            .collect::<Vec<_>>()
                            .join(", "),

                        url: v.distribution.download_url.clone(),
                    });
                }
            }

            Some(versions)
        })
        .collect::<Vec<_>>()
        .into_iter()
        .flat_map(|v| v.into_iter())
        .collect::<Vec<_>>();

    Ok(available_packages)
}

/// Returns the download info of the packages, on error returns all the available packages
/// i.e. (("foo/python", "wapm.io"), ("bar/python" "wapm.io")))
pub fn query_package_from_registry(
    registry_url: &str,
    name: &str,
    version: Option<&str>,
) -> Result<PackageDownloadInfo, QueryPackageError> {
    let available_packages = query_available_packages_from_registry(registry_url, name)?;

    if !name.contains('/') {
        return Err(QueryPackageError::AmbigouusName {
            name: name.to_string(),
            packages: available_packages,
        });
    }

    let mut queried_packages = available_packages
        .iter()
        .filter(|v| {
            if name.contains('/') && v.package != name {
                return false;
            }

            if version.is_some() && version != Some(&v.version) {
                return false;
            }

            true
        })
        .collect::<Vec<_>>();

    let selected_package = match version {
        Some(s) => queried_packages.iter().find(|p| p.version == s),
        None => {
            if let Some(latest) = queried_packages.iter().find(|s| s.is_latest_version) {
                Some(latest)
            } else {
                // sort package by version, select highest
                queried_packages.sort_by_key(|k| semver::Version::parse(&k.version).ok());
                queried_packages.first()
            }
        }
    };

    match selected_package {
        None => {
            return Err(QueryPackageError::NoPackageFound {
                name: name.to_string(),
                version: version.as_ref().map(|s| s.to_string()),
                packages: available_packages,
            });
        }
        Some(s) => Ok((*s).clone()),
    }
}

pub fn get_checkouts_dir() -> Option<PathBuf> {
    Some(PartialWapmConfig::get_folder().ok()?.join("checkouts"))
}

/// Returs the path to the directory where all packages on this computer are being stored
pub fn get_global_install_dir(registry_host: &str) -> Option<PathBuf> {
    Some(get_checkouts_dir()?.join(registry_host))
}

pub fn download_and_unpack_targz(url: &str, target_path: &Path) -> Result<PathBuf, String> {
    let target_targz_path = target_path.to_path_buf().join("package.tar.gz");

    let mut resp =
        reqwest::blocking::get(url).map_err(|e| format!("failed to download {url}: {e}"))?;

    if !target_targz_path.exists() {
        // create all the parent paths, only remove the created directory, not the parent dirs
        let _ = std::fs::create_dir_all(&target_targz_path);
        let _ = std::fs::remove_dir(&target_targz_path);
    }

    {
        let mut file = std::fs::File::create(&target_targz_path).map_err(|e| {
            format!(
                "failed to download {url} into {}: {e}",
                target_targz_path.display()
            )
        })?;

        reqwest::blocking::get(url).map_err(|e| format!("{e}"))?;

        resp.copy_to(&mut file).map_err(|e| format!("{e}"))?;
    }

    let file = std::fs::File::open(&target_targz_path).map_err(|e| {
        format!(
            "failed to download {url} into {}: {e}",
            target_targz_path.display()
        )
    })?;

    let gz_decoded = flate2::read::GzDecoder::new(file);
    let mut ar = tar::Archive::new(gz_decoded);
    ar.unpack(target_path)
        .map_err(|e| format!("failed to unpack {}: {e}", target_targz_path.display()))?;

    let _ = std::fs::remove_file(target_targz_path);

    Ok(target_path.to_path_buf())
}

/// Given a triple of [registry, name, version], downloads and installs the
/// .tar.gz if it doesn't yet exist, returns the (package dir, entrypoint .wasm file path)
pub fn install_package(
    registry: Option<&str>,
    name: &str,
    version: Option<&str>,
    package_download_info: Option<PackageDownloadInfo>,
    force_install: bool,
) -> Result<(LocalPackage, PathBuf), String> {
    let package_info = match package_download_info {
        Some(s) => s,
        None => {
            let registries = match registry {
                Some(s) => vec![s.to_string()],
                None => get_all_available_registries()?,
            };
            let mut url_of_package = None;
            let mut error_packages = Vec::new();

            let version_str = match version {
                None => name.to_string(),
                Some(v) => format!("{name}@{v}"),
            };

            let registries_searched = registries
                .iter()
                .filter_map(|s| url::Url::parse(s).ok())
                .filter_map(|s| Some(s.host_str()?.to_string()))
                .collect::<Vec<_>>();

            let mut error_str =
                format!("Package {version_str} not found in registries {registries_searched:?}.");

            for r in registries.iter() {
                let registry_test = test_if_registry_present(r);
                if !registry_test.clone().unwrap_or(false) {
                    continue;
                }

                if !force_install {
                    let package_has_new_version = get_if_package_has_new_version(
                        r,
                        name,
                        version.map(|s| s.to_string()),
                        Duration::from_secs(60 * 5),
                    );
                    if let Ok(Some(package)) = package_has_new_version {
                        url_of_package = Some((r, package));
                        break;
                    }
                }

                match query_package_from_registry(r, name, version) {
                    Ok(o) => {
                        url_of_package = Some((r, o));
                        break;
                    }
                    Err(e) => {
                        error_packages.push(e);
                    }
                }
            }

            let mut did_you_mean = error_packages
                .iter()
                .flat_map(|error| {
                    if let QueryPackageError::AmbigouusName { name, packages: _ } = error {
                        error_str = format!("Ambigouus package name {name:?}. Please specify the package in the namespace/name format.");
                    }
                    let packages = error.get_packages();
                    packages.iter().filter_map(|f| {
                        let from = url::Url::parse(&f.registry).ok()?.host_str()?.to_string();
                        Some(format!("     {}@{} (from {from})", f.package, f.version))
                    })
                    .collect::<Vec<_>>()
                    .into_iter()
                })
                .collect::<Vec<_>>();

            let did_you_mean = if did_you_mean.is_empty() {
                String::new()
            } else {
                did_you_mean.sort();
                did_you_mean.dedup();
                format!("\r\n\r\nDid you mean:\r\n{}\r\n", did_you_mean.join("\r\n"))
            };

            let (_, package_info) =
                url_of_package.ok_or_else(|| format!("{error_str}{did_you_mean}"))?;

            package_info
        }
    };

    let host = url::Url::parse(&package_info.registry)
        .map_err(|e| format!("invalid url: {}: {e}", package_info.registry))?
        .host_str()
        .ok_or_else(|| format!("invalid url: {}", package_info.registry))?
        .to_string();

    let dir = get_package_local_dir(&host, &package_info.package, &package_info.version)?;

    let version = package_info.version;
    let name = package_info.package;

    if !dir.join("wapm.toml").exists() || force_install {
        download_and_unpack_targz(&package_info.url, &dir)?;
    }

    Ok((
        LocalPackage {
            registry: package_info.registry,
            name,
            version,
        },
        dir,
    ))
}

pub fn test_if_registry_present(registry: &str) -> Result<bool, String> {
    use crate::graphql::{test_if_registry_present, TestIfRegistryPresent};
    use graphql_client::GraphQLQuery;

    let q = TestIfRegistryPresent::build_query(test_if_registry_present::Variables {});
    let _ = crate::graphql::execute_query_modifier_inner_check_json(
        registry,
        "",
        &q,
        Some(Duration::from_secs(1)),
        |f| f,
    )
    .map_err(|e| format!("{e}"))?;

    Ok(true)
}

pub fn get_all_available_registries() -> Result<Vec<String>, String> {
    let config = PartialWapmConfig::from_file()?;
    let mut registries = Vec::new();
    match config.registry {
        Registries::Single(s) => {
            registries.push(format_graphql(&s.url));
        }
        Registries::Multi(m) => {
            for key in m.tokens.keys() {
                registries.push(format_graphql(key));
            }
        }
    }
    Ok(registries)
}

// TODO: this test is segfaulting only on linux-musl, no other OS
// See https://github.com/wasmerio/wasmer/pull/3215
#[cfg(not(target_env = "musl"))]
#[test]
fn test_install_package() {
    println!("test install package...");
    let registry = "https://registry.wapm.io/graphql";
    if !test_if_registry_present(registry).unwrap_or(false) {
        panic!("registry.wapm.io not reachable, test will fail");
    }
    println!("registry present");

    let wabt = query_package_from_registry(registry, "wasmer/wabt", Some("1.0.29")).unwrap();

    println!("wabt queried: {wabt:#?}");

    assert_eq!(wabt.registry, registry);
    assert_eq!(wabt.package, "wasmer/wabt");
    assert_eq!(wabt.version, "1.0.29");
    assert_eq!(
        wabt.commands,
        "wat2wasm, wast2json, wasm2wat, wasm-interp, wasm-validate, wasm-strip"
    );
    assert_eq!(
        wabt.url,
        "https://registry-cdn.wapm.io/packages/wasmer/wabt/wabt-1.0.29.tar.gz".to_string()
    );

    let (package, _) =
        install_package(Some(registry), "wasmer/wabt", Some("1.0.29"), None, true).unwrap();

    println!("package installed: {package:#?}");

    assert_eq!(
        package.get_path().unwrap(),
        get_global_install_dir("registry.wapm.io")
            .unwrap()
            .join("wasmer")
            .join("wabt")
            .join("1.0.29")
    );

    let all_installed_packages = get_all_local_packages(Some(registry));

    println!("all_installed_packages: {all_installed_packages:#?}");

    let is_installed = all_installed_packages
        .iter()
        .any(|p| p.name == "wasmer/wabt" && p.version == "1.0.29");

    println!("is_installed: {is_installed:#?}");

    if !is_installed {
        let panic_str = get_all_local_packages(Some(registry))
            .iter()
            .map(|p| format!("{} {} {}", p.registry, p.name, p.version))
            .collect::<Vec<_>>()
            .join("\r\n");
        panic!("get all local packages: failed to install:\r\n{panic_str}");
    }

    println!("ok, done");
}
