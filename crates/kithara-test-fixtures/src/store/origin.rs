use std::{
    fmt::Write as _,
    fs::{self, File},
    io::{self, Read as _, Write as _},
    net::{SocketAddr, TcpStream},
    path::{Component, Path, PathBuf},
    sync::OnceLock,
};

use kithara_platform::time::Duration;

use super::disk;

struct Consts;

impl Consts {
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
    const HEADER_BODY_SEPARATOR: &'static [u8] = b"\r\n\r\n";
    const READ_TIMEOUT: Duration = Duration::from_secs(60);
    const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
}

/// HTTP origin of store records. When set, [`file`] fetches from this URL and
/// writes a replica under [`super::STORE_ENV`]; the cache is not a second source.
pub const ORIGIN_ENV: &str = "KITHARA_FIXTURE_ORIGIN";

struct Runtime {
    origin: Option<String>,
    root: PathBuf,
}

/// Local path of one store-relative record.
///
/// With no origin, this is the disk store path and does not require the file
/// to exist. With [`ORIGIN_ENV`], this is the cache replica: a hit returns the
/// cached file, a miss fetches from the origin once and then reads the replica.
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidInput`] for a relative path that escapes
/// the store, a non-loopback origin, or an HTTP origin without
/// [`super::STORE_ENV`]. Returns [`io::ErrorKind::NotFound`] when the origin
/// answers 404. An unreachable origin fails immediately with the origin URL
/// and the reverse-mapping requirement in the message.
pub fn file(relative: &Path) -> io::Result<PathBuf> {
    let relative = relative_path(relative)?;
    let runtime = runtime()?;
    let path = runtime.root.join(relative);
    match runtime.origin.as_deref() {
        None => Ok(path),
        Some(origin) => {
            if complete(&path) {
                return Ok(path);
            }
            let bytes = get(origin, relative)?;
            cache_write(&path, &bytes)?;
            Ok(path)
        }
    }
}

fn runtime() -> io::Result<&'static Runtime> {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    if let Some(runtime) = RUNTIME.get() {
        return Ok(runtime);
    }
    let origin = match std::env::var_os(ORIGIN_ENV) {
        None => None,
        Some(value) => {
            let value = value.to_str().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{ORIGIN_ENV} must be an http://127.0.0.1 URL"),
                )
            })?;
            Some(parse_origin(value)?)
        }
    };
    if origin.is_some() {
        disk::root_from_env()?;
    }
    let root = disk::runtime_root()?.to_owned();
    Ok(RUNTIME.get_or_init(|| Runtime { origin, root }))
}

fn parse_origin(raw: &str) -> io::Result<String> {
    let rest = raw.trim().strip_prefix("http://").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{ORIGIN_ENV} must be an http://127.0.0.1 URL"),
        )
    })?;
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.contains(['/', '?', '#', '@']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{ORIGIN_ENV} must be an http://127.0.0.1 URL"),
        ));
    }
    let (host, port) = rest.rsplit_once(':').ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{ORIGIN_ENV} must be an http://127.0.0.1 URL"),
        )
    })?;
    if host != "127.0.0.1" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{ORIGIN_ENV} must be an http://127.0.0.1 URL"),
        ));
    }
    let port: u16 = port.parse().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{ORIGIN_ENV} must be an http://127.0.0.1 URL"),
        )
    })?;
    if port == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{ORIGIN_ENV} must be an http://127.0.0.1 URL"),
        ));
    }
    Ok(format!("http://127.0.0.1:{port}"))
}

fn relative_path(path: &Path) -> io::Result<&Path> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || !path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "fixture path must remain under its store",
        ));
    }
    Ok(path)
}

fn complete(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.len() != 0)
}

fn cache_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if bytes.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "fixture origin returned an empty record",
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "fixture path must remain under its store",
        )
    })?;
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".{}.tmp.{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::new(
                io::ErrorKind::InvalidData,
                "fixture filename is not UTF-8"
            ))?,
        std::process::id()
    ));
    let write = (|| -> io::Result<()> {
        let mut file = File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if let Err(error) = write {
        drop(fs::remove_file(&tmp));
        return Err(error);
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

fn get(origin: &str, relative: &Path) -> io::Result<Vec<u8>> {
    let encoded = encode_relative(relative)?;
    let host = origin
        .strip_prefix("http://")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "fixture origin is not HTTP"))?;
    let addr: SocketAddr = host.parse().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("fixture origin {origin} is not a socket address: {error}"),
        )
    })?;
    let mut stream = TcpStream::connect_timeout(&addr, Consts::CONNECT_TIMEOUT)
        .map_err(|error| origin_unreachable(origin, &error))?;
    stream
        .set_nodelay(true)
        .map_err(|error| origin_unreachable(origin, &error))?;
    stream
        .set_read_timeout(Some(Consts::READ_TIMEOUT))
        .map_err(|error| origin_unreachable(origin, &error))?;
    stream
        .set_write_timeout(Some(Consts::WRITE_TIMEOUT))
        .map_err(|error| origin_unreachable(origin, &error))?;
    let request =
        format!("GET /store/{encoded} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|error| origin_unreachable(origin, &error))?;
    stream
        .flush()
        .map_err(|error| origin_unreachable(origin, &error))?;
    let mut buffer = Vec::new();
    stream
        .read_to_end(&mut buffer)
        .map_err(|error| origin_unreachable(origin, &error))?;
    parse_body(origin, &buffer)
}

fn encode_relative(relative: &Path) -> io::Result<String> {
    let mut encoded = String::new();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "fixture path must remain under its store",
            ));
        };
        let part = part.to_str().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "fixture path is not UTF-8")
        })?;
        if !encoded.is_empty() {
            encoded.push('/');
        }
        for byte in part.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    encoded.push(char::from(byte));
                }
                _ => {
                    let _ = write!(encoded, "%{byte:02X}");
                }
            }
        }
    }
    Ok(encoded)
}

fn parse_body(origin: &str, response: &[u8]) -> io::Result<Vec<u8>> {
    let split = response
        .windows(Consts::HEADER_BODY_SEPARATOR.len())
        .position(|window| window == Consts::HEADER_BODY_SEPARATOR)
        .map(|index| index + Consts::HEADER_BODY_SEPARATOR.len())
        .ok_or_else(|| {
            origin_unreachable(
                origin,
                &io::Error::new(io::ErrorKind::InvalidData, "HTTP response has no body"),
            )
        })?;
    let headers = std::str::from_utf8(&response[..split]).map_err(|_| {
        origin_unreachable(
            origin,
            &io::Error::new(io::ErrorKind::InvalidData, "HTTP headers are not UTF-8"),
        )
    })?;
    let status = headers.lines().next().unwrap_or_default();
    if status.contains(" 404 ") {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("fixture origin {origin} has no such record"),
        ));
    }
    if !status.contains(" 200 ") {
        return Err(origin_unreachable(
            origin,
            &io::Error::new(io::ErrorKind::InvalidData, format!("HTTP {status}")),
        ));
    }
    let mut body = response[split..].to_vec();
    if let Some(length) = content_length(headers) {
        if body.len() < length {
            return Err(origin_unreachable(
                origin,
                &io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("expected {length} bytes, received {}", body.len()),
                ),
            ));
        }
        body.truncate(length);
    }
    Ok(body)
}

fn content_length(headers: &str) -> Option<usize> {
    headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse().ok())
            .flatten()
    })
}

fn origin_unreachable(origin: &str, error: &io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!(
            "fixture origin {origin} is unreachable ({error}); adb reverse is required for every Android fixture read"
        ),
    )
}
