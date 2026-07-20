use std::{env, ffi::OsStr};

use anyhow::{Result, bail};
use kithara_agent_hook::HookCommand;

fn main() -> Result<()> {
    let mut args = env::args_os().skip(1);
    let command = match args.next().as_deref() {
        Some(value) if value == OsStr::new("pre-bash") => HookCommand::PreBash,
        Some(value) if value == OsStr::new("post-edit") => HookCommand::PostEdit,
        Some(value) => bail!("unknown agent hook command `{}`", value.to_string_lossy()),
        None => bail!("expected `pre-bash` or `post-edit`"),
    };
    if args.next().is_some() {
        bail!("agent hook accepts exactly one command");
    }
    kithara_agent_hook::run(command)
}
