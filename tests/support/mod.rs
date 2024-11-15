#![allow(dead_code)]
use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use anyhow::{bail, Context, Result};
use assert_cmd::prelude::OutputAssertExt;
use cargo_component_core::command::{CACHE_DIR_ENV_VAR, CONFIG_FILE_ENV_VAR};
use indexmap::IndexSet;
use tempfile::TempDir;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use toml_edit::DocumentMut;
use warg_crypto::signing::PrivateKey;
use warg_protocol::operator::NamespaceState;
use warg_server::{policy::content::WasmContentPolicy, Config, Server};
use wasm_pkg_client::{Client, PackageRef, PublishOpts, Registry};
use wasmparser::{Chunk, Encoding, Parser, Payload, Validator};
use wit_parser::{Resolve, UnresolvedPackageGroup};

const WARG_CONFIG_NAME: &str = "warg-config.json";
const WASM_PKG_CONFIG_NAME: &str = "wasm-pkg-config.json";

pub fn test_operator_key() -> &'static str {
    "ecdsa-p256:I+UlDo0HxyBBFeelhPPWmD+LnklOpqZDkrFP5VduASk="
}

pub fn test_signing_key() -> &'static str {
    "ecdsa-p256:2CV1EpLaSYEn4In4OAEDAj5O4Hzu8AFAxgHXuG310Ew="
}

pub fn cargo_component<I, S>(args: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut exe = std::env::current_exe().unwrap();
    exe.pop(); // remove test exe name
    exe.pop(); // remove `deps`
    exe.push("cargo-component");
    exe.set_extension(std::env::consts::EXE_EXTENSION);

    let mut cmd = Command::new(&exe);
    cmd.arg("component");
    cmd.args(args);

    cmd
}

pub async fn publish(
    config: wasm_pkg_client::Config,
    name: &PackageRef,
    version: &str,
    content: Vec<u8>,
) -> Result<()> {
    let client = Client::new(config);

    client
        .publish_release_data(
            Box::pin(std::io::Cursor::new(content)),
            PublishOpts {
                package: Some((name.to_owned(), version.parse().unwrap())),
                ..Default::default()
            },
        )
        .await?;

    Ok(())
}

pub async fn publish_component(
    config: wasm_pkg_client::Config,
    id: &str,
    version: &str,
    wat: &str,
) -> Result<()> {
    publish(
        config,
        &id.parse()?,
        version,
        wat::parse_str(wat).context("failed to parse component for publishing")?,
    )
    .await
}

pub async fn publish_wit(
    config: wasm_pkg_client::Config,
    id: &str,
    version: &str,
    wit: &str,
) -> Result<()> {
    let mut resolve = Resolve::new();
    let pkg = resolve
        .push_group(
            UnresolvedPackageGroup::parse(Path::new("foo.wit"), wit)
                .context("failed to parse wit for publishing")?,
        )
        .context("failed to resolve wit for publishing")?;

    let bytes =
        wit_component::encode(&resolve, pkg).context("failed to encode wit for publishing")?;

    publish(config, &id.parse()?, version, bytes).await
}

pub struct ServerInstance {
    task: Option<JoinHandle<()>>,
    shutdown: CancellationToken,
    root: Rc<TempDir>,
}

impl ServerInstance {
    /// Returns a `Project` that is configured to use the server instance with the correct config.
    pub fn project<I, S>(&self, name: &str, lib: bool, additional_args: I) -> Result<Project>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let proj = Project {
            dir: self.root.clone(),
            root: self.root.path().join(name),
            config_file: Some(self.root.path().join(WASM_PKG_CONFIG_NAME)),
        };

        proj.new_inner(name, lib, additional_args)?;
        Ok(proj)
    }
}

impl Drop for ServerInstance {
    fn drop(&mut self) {
        futures::executor::block_on(async move {
            self.shutdown.cancel();
            self.task.take().unwrap().await.ok();
        });
    }
}

/// Spawns a server as a background task. This will start a
pub async fn spawn_server<I, S>(
    additional_namespaces: I,
) -> Result<(ServerInstance, wasm_pkg_client::Config, Registry)>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let root = Rc::new(TempDir::new().context("failed to create temp dir")?);
    let shutdown = CancellationToken::new();
    let config = Config::new(
        PrivateKey::decode(test_operator_key().to_string())?,
        Some(vec![("test".to_string(), NamespaceState::Defined)]),
        root.path().join("server"),
    )
    .with_addr(([127, 0, 0, 1], 0))
    .with_shutdown(shutdown.clone().cancelled_owned())
    .with_checkpoint_interval(Duration::from_millis(100))
    .with_content_policy(WasmContentPolicy::default());

    let server = Server::new(config).initialize().await?;
    let addr = server.local_addr()?;

    let task = tokio::spawn(async move {
        server.serve().await.unwrap();
    });

    let instance = ServerInstance {
        task: Some(task),
        shutdown,
        root: root.to_owned(),
    };

    let warg_config = warg_client::Config {
        home_url: Some(format!("http://{addr}")),
        registries_dir: Some(root.path().join("registries")),
        content_dir: Some(root.path().join("content")),
        namespace_map_path: Some(root.path().join("namespaces")),
        keys: IndexSet::new(),
        keyring_auth: false,
        keyring_backend: None,
        ignore_federation_hints: false,
        disable_auto_accept_federation_hints: false,
        disable_auto_package_init: false,
        disable_interactive: true,
    };

    let config_file = root.path().join(WARG_CONFIG_NAME);
    warg_config.write_to_file(&config_file)?;

    let mut config = wasm_pkg_client::Config::default();
    // We should probably update wasm-pkg-tools to use http for "localhost" or "127.0.0.1"
    let registry: Registry = format!("localhost:{}", addr.port()).parse().unwrap();
    let registry_mapping = wasm_pkg_client::RegistryMapping::Registry(registry.clone());
    config.set_namespace_registry("test".parse().unwrap(), registry_mapping.clone());
    for ns in additional_namespaces {
        config.set_namespace_registry(ns.as_ref().parse().unwrap(), registry_mapping.clone());
    }
    let reg_conf = config.get_or_insert_registry_config_mut(&registry);
    reg_conf.set_default_backend(Some("warg".to_string()));
    reg_conf
        .set_backend_config(
            "warg",
            wasm_pkg_client::warg::WargRegistryConfig {
                client_config: warg_config,
                auth_token: None,
                signing_key: Some(Arc::new(test_signing_key().to_string().try_into()?)),
                config_file: Some(config_file),
            },
        )
        .expect("Should be able to set backend config");

    config
        .to_file(root.path().join(WASM_PKG_CONFIG_NAME))
        .await?;

    Ok((instance, config, registry))
}

#[derive(Debug)]
pub struct Project {
    pub dir: Rc<TempDir>,
    pub root: PathBuf,
    config_file: Option<PathBuf>,
}

impl Project {
    /// Creates a new project with the given name and whether or not to create a library instead of
    /// a binary. This should only be used if you want an "empty" project that doesn't have things
    /// like warg config or wasm pkg tools config configured. If you want a project with a warg
    /// config and wasm pkg tools config, use the `project` method of `ServerInstance`.
    pub fn new(name: &str, lib: bool) -> Result<Self> {
        let dir = TempDir::new()?;
        let root = dir.path().join(name);
        let proj = Self {
            dir: Rc::new(dir),
            root,
            config_file: None,
        };

        proj.new_inner(name, lib, Vec::<String>::new())?;

        Ok(proj)
    }

    /// Same as `new` but allows you to specify additional arguments to pass to `cargo component
    /// new`
    pub fn new_with_args<I, S>(name: &str, lib: bool, additional_args: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let dir = TempDir::new()?;
        let root = dir.path().join(name);
        let proj = Self {
            dir: Rc::new(dir),
            root,
            config_file: None,
        };

        proj.new_inner(name, lib, additional_args)?;

        Ok(proj)
    }

    fn new_inner<I, S>(&self, name: &str, lib: bool, additional_args: I) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut args = vec!["new".to_string(), name.to_string()];
        if lib {
            args.push("--lib".to_string());
        }
        args.extend(additional_args.into_iter().map(|arg| arg.into()));

        self.cargo_component(args)
            .current_dir(self.dir.path())
            .assert()
            .try_success()?;

        Ok(())
    }

    /// Same as `new` but uses the given temp directory instead of creating a new one.
    pub fn with_dir<I, S>(dir: Rc<TempDir>, name: &str, lib: bool, args: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let root = dir.path().join(name);
        let proj = Self {
            dir,
            root,
            config_file: None,
        };

        proj.new_inner(name, lib, args)?;

        Ok(proj)
    }

    /// Creates a new project that hasn't been initialized yet. This is useful for testing workflows
    /// of `cargo component new`
    pub fn new_uninitialized(dir: Rc<TempDir>, root: PathBuf) -> Self {
        Self {
            dir,
            root,
            config_file: None,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn dir(&self) -> &Rc<TempDir> {
        &self.dir
    }

    pub fn file<B: AsRef<Path>>(&self, path: B, body: impl AsRef<[u8]>) -> Result<&Self> {
        let path = self.root().join(path);
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path, body)?;
        Ok(self)
    }

    pub fn read_manifest(&self) -> Result<DocumentMut> {
        let manifest_path = self.root.join("Cargo.toml");
        let manifest_text = fs::read_to_string(manifest_path)?;
        Ok(manifest_text.parse()?)
    }

    pub fn update_manifest(
        &self,
        f: impl FnOnce(DocumentMut) -> Result<DocumentMut>,
    ) -> Result<()> {
        let manifest = self.read_manifest()?;
        let manifest_path = self.root.join("Cargo.toml");
        fs::write(manifest_path, f(manifest)?.to_string())?;
        Ok(())
    }

    pub fn build_dir(&self) -> PathBuf {
        self.root().join("target")
    }

    pub fn debug_wasm(&self, name: &str) -> PathBuf {
        self.build_dir()
            .join("wasm32-wasip1")
            .join("debug")
            .join(format!("{name}.wasm"))
    }

    pub fn release_wasm(&self, name: &str) -> PathBuf {
        self.build_dir()
            .join("wasm32-wasip1")
            .join("release")
            .join(format!("{name}.wasm"))
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.dir.path().join("cache")
    }

    pub fn config_file(&self) -> Option<&Path> {
        self.config_file.as_deref()
    }

    pub fn cargo_component<I, S>(&self, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut cmd = cargo_component(args);
        // Set the cache dir and the config file env var for every command
        if let Some(config_file) = self.config_file() {
            cmd.env(CONFIG_FILE_ENV_VAR, config_file);
        }
        cmd.env(CACHE_DIR_ENV_VAR, self.cache_dir());
        cmd.current_dir(&self.root);
        cmd
    }
}

pub fn validate_component(path: &Path) -> Result<()> {
    let bytes = fs::read(path)
        .with_context(|| format!("failed to read `{path}`", path = path.display()))?;

    // Validate the bytes as either a component or a module
    Validator::new_with_features(Default::default()).validate_all(&bytes)?;

    // Check that the bytes are for a component and not a module
    let mut parser = Parser::new(0);
    match parser.parse(&bytes, true)? {
        Chunk::Parsed {
            payload:
                Payload::Version {
                    encoding: Encoding::Component,
                    ..
                },
            ..
        } => Ok(()),
        Chunk::Parsed { payload, .. } => {
            bail!("expected component version payload, got {:?}", payload)
        }
        Chunk::NeedMoreData(_) => unreachable!(),
    }
}
