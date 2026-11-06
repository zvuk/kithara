use std::{
    env, fs,
    io::{self, BufRead, BufReader, Write},
    os::unix::net::UnixListener,
    path::PathBuf,
    process::{Command, ExitCode, Stdio},
    time::Duration,
};

pub(super) fn start() -> io::Result<()> {
    let runtime = env::var_os("KACHE_RUNTIME_DIR")
        .ok_or_else(|| io::Error::other("fixture needs KACHE_RUNTIME_DIR"))?;
    let mut child = Command::new(env::current_exe()?)
        .arg("--kache-stats-socket")
        .arg(runtime)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut ready = String::new();
    BufReader::new(
        child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("missing ready pipe"))?,
    )
    .read_line(&mut ready)?;
    if ready != "ready\n" {
        child.wait()?;
        return Err(io::Error::other("fixture socket did not start"));
    }
    Ok(())
}

pub(super) fn serve() -> io::Result<ExitCode> {
    let runtime = PathBuf::from(
        env::args_os()
            .nth(2)
            .ok_or_else(|| io::Error::other("missing fixture runtime"))?,
    );
    let path = runtime.join("daemon.sock");
    if path.exists() {
        fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    println!("ready");
    io::stdout().flush()?;
    let (mut socket, _) = listener.accept()?;
    socket.set_read_timeout(Some(Duration::from_secs(60)))?;
    let mut request = String::new();
    BufReader::new(socket.try_clone()?).read_line(&mut request)?;
    let stats: serde_json::Value =
        serde_json::from_str(&env::var("KITHARA_TEST_STDOUT").unwrap_or_else(|_| "{}".into()))
            .unwrap_or_default();
    let reply = serde_json::json!({"ok":stats["daemon_connected"],"stats":{
        "effective_config":{"remote_description":stats["remote"],
            "remote_readonly":stats["daemon_remote_readonly"],"local_only":false},
        "events":stats,
    }});
    writeln!(socket, "{reply}")?;
    fs::remove_file(path)?;
    Ok(ExitCode::SUCCESS)
}
