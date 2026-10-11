use prost::Message;
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};
use tempfile::TempDir;

pub const CLEAN: i32 = 0;
pub const REJECTED: i32 = 2;

pub fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_pixel-uecaps-toolbox"))
}

pub fn fixture() -> TempDir {
    tempfile::tempdir().expect("fixture directory")
}

pub fn write_message(path: &Path, message: &impl Message) {
    fs::write(path, message.encode_to_vec()).unwrap();
}

pub fn stdout(out: &Output) -> &str {
    std::str::from_utf8(&out.stdout).expect("UTF-8 stdout")
}

pub fn stderr(out: &Output) -> &str {
    std::str::from_utf8(&out.stderr).expect("UTF-8 stderr")
}
