#![allow(dead_code)]

use anyhow::{anyhow, bail, Context, Result};
use assert_cmd::prelude::OutputAssertExt;
use std::{
    env, fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicUsize, Ordering::SeqCst},
};
use warg_client::{
    api,
    storage::{ContentStorage, PublishEntry, PublishInfo},
    FileSystemClient,
};
use wasmparser::{Chunk, Encoding, Parser, Payload, Validator, WasmFeatures};
use wit_parser::{Resolve, UnresolvedPackage};

pub fn test_operator_key() -> &'static str {
    "ecdsa-p256:I+UlDo0HxyBBFeelhPPWmD+LnklOpqZDkrFP5VduASk="
}

pub fn test_signing_key() -> &'static str {
    "ecdsa-p256:2CV1EpLaSYEn4In4OAEDAj5O4Hzu8AFAxgHXuG310Ew="
}

pub fn root() -> Result<PathBuf> {
    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
    std::thread_local! {
        static TEST_ID: usize = NEXT_ID.fetch_add(1, SeqCst);
    }
    let id = TEST_ID.with(|n| *n);
    let mut path = env::current_exe()?;
    path.pop(); // remove test exe name
    path.pop(); // remove `deps`
    path.pop(); // remove `debug` or `release`
    path.push("tests");
    fs::create_dir_all(&path)?;
    Ok(path.join(format!("t{id}")))
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

pub struct WargServer(Child);

impl Drop for WargServer {
    fn drop(&mut self) {
        self.0.kill().ok();
    }
}

fn find_free_port() -> Option<u16> {
    // Attempt to find a free port
    // Note that there will be a tiny window of opportunity for another
    // process to grab this port before the server starts listening on it.
    Some(
        TcpListener::bind("127.0.0.1:0")
            .ok()?
            .local_addr()
            .ok()?
            .port(),
    )
}

pub async fn publish_component(
    config: &warg_client::Config,
    name: &str,
    version: &str,
    wat: &str,
    init: bool,
) -> Result<()> {
    let client = FileSystemClient::new_with_config(None, config)?;

    let bytes = wat::parse_str(wat).context("failed to parse component for publishing")?;

    let digest = client
        .content()
        .store_content(
            Box::pin(futures::stream::once(async move { Ok(bytes.into()) })),
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

    client
        .publish_with_info(
            &test_signing_key().parse().unwrap(),
            PublishInfo {
                package: name.to_string(),
                entries,
            },
        )
        .await
        .context("failed to publish component")?;

    Ok(())
}

pub async fn publish_wit(
    config: &warg_client::Config,
    name: &str,
    version: &str,
    wit: &str,
    init: bool,
) -> Result<()> {
    let client = FileSystemClient::new_with_config(None, config)?;

    let mut resolve = Resolve::new();
    let pkg = resolve
        .push(
            UnresolvedPackage::parse(Path::new("foo.wit"), wit)
                .context("failed to parse wit for publishing")?,
            &Default::default(),
        )
        .context("failed to resolve wit for publishing")?;

    let bytes =
        wit_component::encode(&resolve, pkg).context("failed to encode wit for publishing")?;

    let digest = client
        .content()
        .store_content(
            Box::pin(futures::stream::once(async move { Ok(bytes.into()) })),
            None,
        )
        .await
        .context("failed to store wit component for publishing")?;

    let mut entries = Vec::with_capacity(2);
    if init {
        entries.push(PublishEntry::Init);
    }
    entries.push(PublishEntry::Release {
        version: version.parse().unwrap(),
        content: digest,
    });

    client
        .publish_with_info(
            &test_signing_key().parse().unwrap(),
            PublishInfo {
                package: name.to_string(),
                entries,
            },
        )
        .await
        .context("failed to publish wit component")?;

    Ok(())
}

/// Starts a warg server in a background process.
///
/// Returns a drop handle for the server that will kill the server process and
/// also a `warg_client::Config` that can be used to connect to the server.
pub async fn start_warg_server() -> Result<(WargServer, warg_client::Config)> {
    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
    std::thread_local! {
        static TEST_ID: usize = NEXT_ID.fetch_add(1, SeqCst);
    }

    let id = TEST_ID.with(|n| *n);

    let mut path = env::current_exe()?;
    path.pop(); // remove test exe name
    path.pop(); // remove `deps`
    path.pop(); // remove `debug` or `release`
    path.push("tests");
    path.push(format!("s{id}"));

    drop(fs::remove_dir_all(&path));

    let server_content_dir = path.join("server");
    fs::create_dir_all(&server_content_dir)?;

    let packages_dir = path.join("packages");
    fs::create_dir_all(&packages_dir)?;

    let content_dir = path.join("content");
    fs::create_dir_all(&content_dir)?;

    let mut cmd = Command::new("warg-server");

    let port = find_free_port()
        .ok_or_else(|| anyhow!("failed to find free port for the server to listen on"))?;
    cmd.arg(format!(
        "--content-dir={path}",
        path = server_content_dir.display()
    ));
    cmd.arg(format!("--listen=127.0.0.1:{port}"));

    // For now, use a dummy operator key for the server
    cmd.env("WARG_DEMO_OPERATOR_KEY", test_operator_key());

    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());

    let server = WargServer(
        cmd.spawn()
            .context("failed to start warg-server; is it installed?")?,
    );

    let url = format!("http://127.0.0.1:{port}");
    let client = api::Client::new(&url)?;

    // Attempt to wait for the server to start listening (up to 2.5 seconds)
    // This isn't perfect, but it's better than nothing
    let mut started = false;
    for _ in 0..10 {
        if client.latest_checkpoint().await.is_ok() {
            started = true;
            break;
        }

        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }

    if !started {
        bail!("failed to start warg-server (timeout)");
    }

    let config = warg_client::Config {
        default_url: Some(url),
        registries_dir: Some(packages_dir),
        content_dir: Some(content_dir),
    };

    Ok((server, config))
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

        cargo_component(&format!("new --lib {name}"))
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
        cargo_component(&format!("new --lib {name} {args}"))
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
        Chunk::Parsed { payload, .. } => Err(anyhow::anyhow!(
            "expected component version payload, got {:?}",
            payload
        )),
        Chunk::NeedMoreData(_) => unreachable!(),
    }
}
