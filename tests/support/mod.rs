#![allow(dead_code)]

use anyhow::{Context, Result};
use assert_cmd::prelude::OutputAssertExt;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicUsize, Ordering::SeqCst},
};
use toml_edit::{value, Document, InlineTable, Value};
use wasmparser::{Chunk, Encoding, Parser, Payload, Validator, WasmFeatures};

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

        cargo_component(&format!("new {name}"))
            .current_dir(&root)
            .assert()
            .success();

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
            .join("wasm32-unknown-unknown")
            .join("debug")
            .join(format!("{name}.wasm"))
    }

    pub fn release_wasm(&self, name: &str) -> PathBuf {
        self.build_dir()
            .join("wasm32-unknown-unknown")
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

#[allow(clippy::too_many_arguments)]
pub fn create_project_with_registry(
    root: &Path,
    registry: &str,
    name: &str,
    package: &str,
    version: &str,
    world: Option<&str>,
    dependency: Option<(&str, &str)>,
    source: &str,
) -> Result<Project> {
    cargo_component(&format!("new {name}"))
        .current_dir(root)
        .assert()
        .success();

    let project = ProjectBuilder::new(root.join(name)).build();

    let manifest_path = project.root().join("Cargo.toml");
    let mut manifest: Document = fs::read_to_string(&manifest_path)?.parse()?;

    let target = &mut manifest["package"]["metadata"]["component"]["target"];
    target.as_table_like_mut().unwrap().remove("path");
    target["package"] = value(package);
    target["version"] = value(version);
    if let Some(world) = world {
        target["world"] = value(world);
    }

    let registries = &mut manifest["package"]["metadata"]["component"]["registries"];
    registries["default"] = value(InlineTable::from_iter([("path", Value::from(registry))]));

    let dependencies = &mut manifest["package"]["metadata"]["component"]["dependencies"];
    if let Some((name, package)) = dependency {
        dependencies[name] = value(package);
    }

    fs::write(manifest_path, manifest.to_string())?;
    project.file("src/lib.rs", source)?;

    Ok(project)
}
