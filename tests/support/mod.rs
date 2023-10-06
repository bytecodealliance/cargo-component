#![allow(dead_code)]

use anyhow::{bail, Context, Result};
use assert_cmd::prelude::OutputAssertExt;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicUsize, Ordering::SeqCst},
    time::Duration,
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use toml_edit::{value, Document, InlineTable};
use warg_client::{
    storage::{ContentStorage, PublishEntry, PublishInfo},
    FileSystemClient,
};
use warg_crypto::signing::PrivateKey;
use warg_protocol::registry::PackageId;
use warg_server::{policy::content::WasmContentPolicy, Config, Server};
use wasmparser::{Chunk, Encoding, Parser, Payload, Validator, WasmFeatures};
use wit_parser::{Resolve, UnresolvedPackage};

pub fn test_operator_key() -> &'static str {
    "ecdsa-p256:I+UlDo0HxyBBFeelhPPWmD+LnklOpqZDkrFP5VduASk="
}

pub fn test_signing_key() -> &'static str {
    "ecdsa-p256:2CV1EpLaSYEn4In4OAEDAj5O4Hzu8AFAxgHXuG310Ew="
}

pub fn redirect_bindings_crate(doc: &mut Document) {
    const PATH_TO_BINDINGS_CRATE: &str = "../../../../../crates/bindings";

    doc["dependencies"]["cargo-component-bindings"] =
        value(InlineTable::from_iter([("path", PATH_TO_BINDINGS_CRATE)]));
}

pub fn root() -> Result<PathBuf> {
    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
    std::thread_local! {
        static TEST_ID: usize = NEXT_ID.fetch_add(1, SeqCst);
    }
    let id = TEST_ID.with(|n| *n);
    let mut path: PathBuf = env!("CARGO_TARGET_TMPDIR").into();
    path.push(format!(
        "cargo-component-{test}-{id}",
        test = env!("CARGO_CRATE_NAME")
    ));
    fs::create_dir_all(&path)?;

    let manifest = path.join("Cargo.toml");

    // This works around an apparent bug in cargo where
    // a directory is explicitly excluded from a workspace,
    // but `cargo new` still detects `workspace.package` settings
    // and sets them to be inherited in the new project.
    fs::write(
        &manifest,
        r#"
[workspace]
exclude = ["test"]
"#,
    )
    .with_context(|| format!("failed to write `{path}`", path = manifest.display()))?;

    path.push("test");
    fs::create_dir_all(&path)?;

    Ok(path)
}

pub fn create_root() -> Result<PathBuf> {
    let root = root()?;
    drop(fs::remove_dir_all(&root));
    fs::create_dir_all(&root)?;
    Ok(root)
}

pub fn cargo_component(args: &str) -> Command {
    let mut exe = std::env::current_exe().unwrap();
    exe.pop(); // remove test exe name
    exe.pop(); // remove `deps`
    exe.push("cargo-component");
    exe.set_extension(std::env::consts::EXE_EXTENSION);

    let mut cmd = Command::new(&exe);
    cmd.arg("component");
    for arg in args.split_whitespace() {
        cmd.arg(arg);
    }

    cmd
}

pub fn project() -> Result<ProjectBuilder> {
    Ok(ProjectBuilder::new(create_root()?))
}

pub async fn publish(
    config: &warg_client::Config,
    id: &PackageId,
    version: &str,
    content: Vec<u8>,
    init: bool,
) -> Result<()> {
    let client = FileSystemClient::new_with_config(None, config)?;

    let digest = client
        .content()
        .store_content(
            Box::pin(futures::stream::once(async move { Ok(content.into()) })),
            None,
        )
        .await
        .context("failed to store component for publishing")?;

    let mut entries = Vec::with_capacity(2);
    if init {
        entries.push(PublishEntry::Init);
    }
    entries.push(PublishEntry::Release {
        version: version.parse().unwrap(),
        content: digest,
    });

    let record_id = client
        .publish_with_info(
            &PrivateKey::decode(test_signing_key().to_string()).unwrap(),
            PublishInfo {
                id: id.clone(),
                head: None,
                entries,
            },
        )
        .await
        .context("failed to publish component")?;

    client
        .wait_for_publish(id, &record_id, Duration::from_secs(1))
        .await?;

    Ok(())
}

pub async fn publish_component(
    config: &warg_client::Config,
    id: &str,
    version: &str,
    wat: &str,
    init: bool,
) -> Result<()> {
    publish(
        config,
        &id.parse()?,
        version,
        wat::parse_str(wat).context("failed to parse component for publishing")?,
        init,
    )
    .await
}

pub async fn publish_wit(
    config: &warg_client::Config,
    id: &str,
    version: &str,
    wit: &str,
    init: bool,
) -> Result<()> {
    let mut resolve = Resolve::new();
    let pkg = resolve
        .push(
            UnresolvedPackage::parse(Path::new("foo.wit"), wit)
                .context("failed to parse wit for publishing")?,
        )
        .context("failed to resolve wit for publishing")?;

    let bytes =
        wit_component::encode(&resolve, pkg).context("failed to encode wit for publishing")?;

    publish(config, &id.parse()?, version, bytes, init).await
}

pub struct ServerInstance {
    task: Option<JoinHandle<()>>,
    shutdown: CancellationToken,
}

impl Drop for ServerInstance {
    fn drop(&mut self) {
        futures::executor::block_on(async move {
            self.shutdown.cancel();
            self.task.take().unwrap().await.ok();
        });
    }
}

/// Spawns a server as a background task.
pub async fn spawn_server(root: &Path) -> Result<(ServerInstance, warg_client::Config)> {
    let shutdown = CancellationToken::new();
    let config = Config::new(
        PrivateKey::decode(test_operator_key().to_string())?,
        root.join("server"),
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
    };

    let config = warg_client::Config {
        default_url: Some(format!("http://{addr}")),
        registries_dir: Some(root.join("registries")),
        content_dir: Some(root.join("content")),
    };

    Ok((instance, config))
}

pub struct Project {
    root: PathBuf,
}

pub struct ProjectBuilder {
    project: Project,
}

impl ProjectBuilder {
    pub fn new(root: PathBuf) -> Self {
        Self {
            project: Project { root },
        }
    }

    pub fn root(&self) -> &Path {
        self.project.root()
    }

    pub fn file<B: AsRef<Path>>(&mut self, path: B, body: &str) -> Result<&mut Self> {
        let path = self.root().join(path);
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(self.root().join(path), body)?;
        Ok(self)
    }

    pub fn build(&mut self) -> Project {
        Project {
            root: self.project.root.clone(),
        }
    }
}

impl Project {
    pub fn new(name: &str) -> Result<Self> {
        let root = create_root()?;

        cargo_component(&format!("new --reactor {name}"))
            .current_dir(&root)
            .assert()
            .try_success()?;

        Ok(Self {
            root: root.join(name),
        })
    }

    pub fn new_bin(name: &str) -> Result<Self> {
        let root = create_root()?;

        cargo_component(&format!("new {name}"))
            .current_dir(&root)
            .assert()
            .try_success()?;

        Ok(Self {
            root: root.join(name),
        })
    }

    pub fn with_root(root: &Path, name: &str, args: &str) -> Result<Self> {
        cargo_component(&format!("new --reactor {name} {args}"))
            .current_dir(root)
            .assert()
            .try_success()?;

        Ok(Self {
            root: root.join(name),
        })
    }

    pub fn file<B: AsRef<Path>>(&self, path: B, body: &str) -> Result<&Self> {
        let path = self.root().join(path);
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(self.root().join(path), body)?;
        Ok(self)
    }

    pub fn update_manifest(&self, f: impl FnOnce(Document) -> Result<Document>) -> Result<()> {
        let manifest_path = self.root.join("Cargo.toml");
        let manifest = fs::read_to_string(&manifest_path)?;
        fs::write(manifest_path, f(manifest.parse()?)?.to_string())?;
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn build_dir(&self) -> PathBuf {
        self.root().join("target")
    }

    pub fn debug_wasm(&self, name: &str) -> PathBuf {
        self.build_dir()
            .join("wasm32-wasi")
            .join("debug")
            .join(format!("{name}.wasm"))
    }

    pub fn release_wasm(&self, name: &str) -> PathBuf {
        self.build_dir()
            .join("wasm32-wasi")
            .join("release")
            .join(format!("{name}.wasm"))
    }

    pub fn cargo_component(&self, cmd: &str) -> Command {
        let mut cmd = cargo_component(cmd);
        cmd.current_dir(&self.root);
        cmd
    }
}

pub fn validate_component(path: &Path) -> Result<()> {
    let bytes = fs::read(path)
        .with_context(|| format!("failed to read `{path}`", path = path.display()))?;

    // Validate the bytes as either a component or a module
    Validator::new_with_features(WasmFeatures {
        component_model: true,
        ..Default::default()
    })
    .validate_all(&bytes)?;

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
