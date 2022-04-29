#![allow(dead_code)]

use anyhow::Result;
use assert_cmd::prelude::OutputAssertExt;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicUsize, Ordering::SeqCst},
};

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
    Ok(path.join(&format!("t{}", id)))
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
    ProjectBuilder::new(root()?)
}

pub struct Project {
    root: PathBuf,
}

pub struct ProjectBuilder {
    project: Project,
}

impl ProjectBuilder {
    pub fn new(root: PathBuf) -> Result<Self> {
        drop(fs::remove_dir_all(&root));
        fs::create_dir_all(&root)?;
        Ok(Self {
            project: Project { root },
        })
    }

    pub fn root(&self) -> PathBuf {
        self.project.root()
    }

    pub fn file<B: AsRef<Path>>(&mut self, path: B, body: &str) -> Result<&mut Self> {
        let path = self.root().join(path);
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(self.root().join(path), body)?;
        Ok(self)
    }

    pub fn build(&mut self) -> Result<Project> {
        Ok(Project {
            root: self.project.root.clone(),
        })
    }
}

impl Project {
    pub fn new(name: &str) -> Result<Self> {
        let root = root()?;
        drop(fs::remove_dir_all(&root));
        fs::create_dir_all(&root)?;

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

    pub fn root(&self) -> PathBuf {
        self.root.clone()
    }

    pub fn build_dir(&self) -> PathBuf {
        self.root().join("target")
    }

    pub fn debug_wasm(&self, name: &str) -> PathBuf {
        self.build_dir()
            .join("wasm32-unknown-unknown")
            .join("debug")
            .join(format!("{}.wasm", name))
    }

    pub fn release_wasm(&self, name: &str) -> PathBuf {
        self.build_dir()
            .join("wasm32-unknown-unknown")
            .join("release")
            .join(format!("{}.wasm", name))
    }

    pub fn cargo_component(&self, cmd: &str) -> Command {
        let mut cmd = cargo_component(cmd);
        cmd.current_dir(&self.root)
            .env("CARGO_HOME", self.root.join("cargo-home"));

        cmd
    }
}
