#![allow(dead_code)]

use anyhow::{bail, Context, Result};
use assert_cmd::prelude::OutputAssertExt;
use indexmap::IndexSet;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
    time::Duration,
};
use tempfile::TempDir;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use toml_edit::Document;
use warg_crypto::signing::PrivateKey;
use warg_protocol::operator::NamespaceState;
use warg_server::{policy::content::WasmContentPolicy, Config, Server};
use wasmparser::{Chunk, Encoding, Parser, Payload, Validator, WasmFeatures};

pub fn test_operator_key() -> &'static str {
    "ecdsa-p256:I+UlDo0HxyBBFeelhPPWmD+LnklOpqZDkrFP5VduASk="
}

pub fn test_signing_key() -> &'static str {
    "ecdsa-p256:2CV1EpLaSYEn4In4OAEDAj5O4Hzu8AFAxgHXuG310Ew="
}

// This works around an apparent bug in cargo where
// a directory is explicitly excluded from a workspace,
// but `cargo new` still detects `workspace.package` settings
// and sets them to be inherited in the new project.
fn exclude_test_directories() -> Result<()> {
    let mut path = env::current_exe()?;
    path.pop(); // remove test exe name
    path.pop(); // remove `deps`
    path.pop(); // remove `debug` or `release`
    path.push("tests");
    path.push("Cargo.toml");

    if !path.exists() {
        fs::write(
            &path,
            r#"
    [workspace]
    exclude = ["cargo-component", "wit"]
    "#,
        )
        .with_context(|| format!("failed to write `{path}`", path = path.display()))?;
    }

    Ok(())
}

pub fn wit(args: &str) -> Command {
    let mut exe = std::env::current_exe().unwrap();
    exe.pop(); // remove test exe name
    exe.pop(); // remove `deps`
    exe.push("wit");
    exe.set_extension(std::env::consts::EXE_EXTENSION);

    let mut cmd = Command::new(&exe);
    for arg in args.split_whitespace() {
        cmd.arg(arg);
    }

    cmd
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
        Some(
            [("test".to_string(), NamespaceState::Defined)]
                .into_iter()
                .collect(),
        ),
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
        home_url: Some(format!("http://{addr}")),
        registries_dir: Some(root.join("registries")),
        content_dir: Some(root.join("content")),
        namespace_map_path: Some(root.join("namespaces")),
        keys: IndexSet::new(),
        keyring_auth: false,
    };

    Ok((instance, config))
}

pub struct Project {
    dir: Rc<TempDir>,
    root: PathBuf,
}

impl Project {
    pub fn new(name: &str) -> Result<Self> {
        let dir = TempDir::new()?;

        wit(&format!("init {name}"))
            .current_dir(&dir)
            .assert()
            .try_success()?;

        let root = dir.path().join(name);

        Ok(Self {
            dir: Rc::new(dir),
            root,
        })
    }

    pub fn with_dir(dir: Rc<TempDir>, name: &str, args: &str) -> Result<Self> {
        wit(&format!("init {name} {args}"))
            .current_dir(dir.as_ref())
            .assert()
            .try_success()?;

        let root = dir.path().join(name);

        Ok(Self { dir, root })
    }

    pub fn file<B: AsRef<Path>>(&self, path: B, body: &str) -> Result<&Self> {
        let path = self.root().join(path);
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(self.root().join(path), body)?;
        Ok(self)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn dir(&self) -> &Rc<TempDir> {
        &self.dir
    }

    pub fn wit(&self, cmd: &str) -> Command {
        let mut cmd = wit(cmd);
        cmd.current_dir(&self.root);
        cmd
    }

    pub fn update_manifest(&self, f: impl FnOnce(Document) -> Result<Document>) -> Result<()> {
        let manifest_path = self.root.join("wit.toml");
        let manifest = fs::read_to_string(&manifest_path)?;
        fs::write(manifest_path, f(manifest.parse()?)?.to_string())?;
        Ok(())
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
