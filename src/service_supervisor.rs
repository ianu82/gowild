use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const COMMAND: &str = "__gowild-service-supervisor";
const LEASE_VERSION: u32 = 1;
const START_WAIT: Duration = Duration::from_secs(30);
const START_POLL: Duration = Duration::from_millis(10);
const MAX_START_BYTES: u64 = 1024;
static NEXT_CONTROL_TEMP: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServiceSupervisorLease {
    pub(crate) version: u32,
    pub(crate) instance_id: String,
    pub(crate) pid: u32,
    pub(crate) started_at_unix_millis: u64,
}

struct SupervisorArgs {
    instance_id: String,
    lease_path: PathBuf,
    start_path: PathBuf,
    argv: Vec<String>,
}

pub(crate) fn maybe_run(args: &[String]) -> Option<io::Result<i32>> {
    (args.get(1).map(String::as_str) == Some(COMMAND)).then(|| parse_args(&args[2..]).and_then(run))
}

#[allow(
    dead_code,
    reason = "task service consumer lands in the next stacked PR"
)]
pub(crate) fn command(
    executable: &Path,
    instance_id: &str,
    lease_path: &Path,
    start_path: &Path,
    argv: &[String],
) -> io::Result<std::process::Command> {
    let lease_path = lease_path
        .to_str()
        .ok_or_else(|| invalid_input("service supervisor lease path is not UTF-8"))?;
    let start_path = start_path
        .to_str()
        .ok_or_else(|| invalid_input("service supervisor start path is not UTF-8"))?;
    let mut encoded = vec![
        instance_id.to_string(),
        lease_path.to_string(),
        start_path.to_string(),
        "--".into(),
    ];
    encoded.extend_from_slice(argv);
    parse_args(&encoded)?;
    let mut command = std::process::Command::new(executable);
    command
        .arg(COMMAND)
        .arg(instance_id)
        .arg(lease_path)
        .arg(start_path)
        .arg("--")
        .args(argv);
    crate::platform::configure_service_supervisor_command(&mut command);
    Ok(command)
}

#[allow(
    dead_code,
    reason = "task service consumer lands in the next stacked PR"
)]
pub(crate) fn write_start_signal(path: &Path, instance_id: &str) -> io::Result<()> {
    if !valid_instance_id(instance_id) {
        return Err(invalid_input("service supervisor instance id is invalid"));
    }
    match fs::symlink_metadata(path) {
        Ok(_) => {
            let existing = read_bounded_regular_file(
                path,
                MAX_START_BYTES,
                "service supervisor start signal",
            )?;
            if String::from_utf8_lossy(&existing).trim() == instance_id {
                return Ok(());
            }
            return Err(invalid_data(
                "service supervisor start signal belongs to another instance",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let bytes = format!("{instance_id}\n");
    match write_private_noclobber(path, "start", bytes.as_bytes()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let existing = read_bounded_regular_file(
                path,
                MAX_START_BYTES,
                "service supervisor start signal",
            )?;
            if String::from_utf8_lossy(&existing).trim() == instance_id {
                Ok(())
            } else {
                Err(invalid_data(
                    "service supervisor start signal belongs to another instance",
                ))
            }
        }
        Err(error) => Err(error),
    }
}

fn parse_args(args: &[String]) -> io::Result<SupervisorArgs> {
    let separator = args.iter().position(|argument| argument == "--");
    let Some(separator) = separator else {
        return Err(invalid_input(
            "service supervisor command has no argv separator",
        ));
    };
    if separator != 3 || args.len() <= separator + 1 {
        return Err(invalid_input("service supervisor command is incomplete"));
    }
    let instance_id = args[0].clone();
    if !valid_instance_id(&instance_id) {
        return Err(invalid_input("service supervisor instance id is invalid"));
    }
    let lease_path = PathBuf::from(&args[1]);
    let start_path = PathBuf::from(&args[2]);
    validate_control_paths(&lease_path, &start_path)?;
    let argv = args[separator + 1..].to_vec();
    if argv[0].trim().is_empty() || argv.iter().any(|argument| argument.contains('\0')) {
        return Err(invalid_input("service supervisor argv is invalid"));
    }
    Ok(SupervisorArgs {
        instance_id,
        lease_path,
        start_path,
        argv,
    })
}

fn run(args: SupervisorArgs) -> io::Result<i32> {
    require_missing_control_path(&args.lease_path)?;
    require_missing_control_path(&args.start_path)?;
    let pid = std::process::id();
    let started_at_unix_millis = crate::platform::process_started_at_unix_millis(pid)
        .ok_or_else(|| io::Error::other("service supervisor process identity is unavailable"))?;
    let lease = ServiceSupervisorLease {
        version: LEASE_VERSION,
        instance_id: args.instance_id.clone(),
        pid,
        started_at_unix_millis,
    };
    write_lease(&args.lease_path, &lease)?;
    if read_lease(&args.lease_path)? != lease {
        let _ = fs::remove_file(&args.lease_path);
        return Err(invalid_data(
            "service supervisor lease did not round-trip exactly",
        ));
    }
    if let Err(error) = wait_for_start(&args.start_path, &args.instance_id, START_WAIT) {
        let _ = fs::remove_file(&args.lease_path);
        return Err(error);
    }
    let _ = fs::remove_file(&args.start_path);
    let result = crate::platform::run_supervised_service(&args.argv[0], &args.argv[1..]);
    let _ = fs::remove_file(&args.lease_path);
    result
}

pub(crate) fn read_lease(path: &Path) -> io::Result<ServiceSupervisorLease> {
    let bytes = read_bounded_regular_file(path, 4096, "service supervisor lease")?;
    let lease: ServiceSupervisorLease = serde_json::from_slice(&bytes)
        .map_err(|_| invalid_data("service supervisor lease is invalid"))?;
    if lease.version != LEASE_VERSION
        || !valid_instance_id(&lease.instance_id)
        || lease.pid == 0
        || lease.started_at_unix_millis == 0
    {
        return Err(invalid_data("service supervisor lease is invalid"));
    }
    Ok(lease)
}

fn write_lease(path: &Path, lease: &ServiceSupervisorLease) -> io::Result<()> {
    let bytes = serde_json::to_vec(lease)
        .map_err(|_| invalid_data("service supervisor lease could not be serialized"))?;
    write_private_noclobber(path, "lease", &bytes)
}

fn write_private_noclobber(path: &Path, prefix: &str, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid_input("service supervisor control file has no parent"))?;
    let (mut file, temporary) = create_private_control_temp(parent, prefix)?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::hard_link(&temporary, path)?;
        fs::remove_file(&temporary)?;
        sync_parent(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn create_private_control_temp(parent: &Path, prefix: &str) -> io::Result<(fs::File, PathBuf)> {
    for _ in 0..100 {
        let nonce = NEXT_CONTROL_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(".{prefix}-{}-{nonce}.tmp", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        match options.open(&path) {
            Ok(file) => return Ok((file, path)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "service supervisor could not allocate a private control temporary file",
    ))
}

fn wait_for_start(path: &Path, instance_id: &str, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        match fs::symlink_metadata(path) {
            Ok(metadata)
                if !metadata.file_type().is_symlink()
                    && metadata.is_file()
                    && metadata.len() <= MAX_START_BYTES =>
            {
                let bytes = read_bounded_regular_file(
                    path,
                    MAX_START_BYTES,
                    "service supervisor start signal",
                )?;
                if String::from_utf8_lossy(&bytes).trim() == instance_id {
                    return Ok(());
                }
            }
            Ok(_) => return Err(invalid_data("service supervisor start signal is unsafe")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "service supervisor start signal timed out",
            ));
        }
        std::thread::sleep(START_POLL);
    }
}

fn read_bounded_regular_file(path: &Path, max_bytes: u64, label: &str) -> io::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > max_bytes {
        return Err(invalid_data_owned(format!(
            "{label} is not a bounded regular file"
        )));
    }
    let file = fs::File::open(path)?;
    let opened = file.metadata()?;
    if !opened.is_file() || opened.len() > max_bytes {
        return Err(invalid_data_owned(format!(
            "{label} is not a bounded regular file"
        )));
    }
    let capacity = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    let mut bytes = Vec::with_capacity(capacity.min(4096));
    file.take(max_bytes + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(invalid_data_owned(format!(
            "{label} exceeds its size limit"
        )));
    }
    Ok(bytes)
}

fn validate_control_paths(lease: &Path, start: &Path) -> io::Result<()> {
    for path in [lease, start] {
        if !path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
        {
            return Err(invalid_input(
                "service supervisor control paths must be absolute",
            ));
        }
    }
    if lease == start || lease.parent() != start.parent() {
        return Err(invalid_input(
            "service supervisor control paths must be distinct siblings",
        ));
    }
    let parent = lease
        .parent()
        .ok_or_else(|| invalid_input("service supervisor control paths have no parent"))?;
    let metadata = fs::symlink_metadata(parent)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(invalid_input(
            "service supervisor control directory is unsafe",
        ));
    }
    Ok(())
}

fn require_missing_control_path(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(invalid_data("service supervisor control path is unsafe"))
        }
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "service supervisor control path already exists",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn valid_instance_id(value: &str) -> bool {
    (16..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn invalid_data_owned(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_preserves_direct_argv_and_validated_control_paths() {
        let root = std::env::temp_dir().join("gowild-supervisor-command-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let executable = std::env::current_exe().unwrap();
        let lease_path = root.join("lease.json");
        let start_path = root.join("start");
        let command = command(
            &executable,
            "1234567890abcdef-service",
            &lease_path,
            &start_path,
            &["program".into(), "argument with spaces".into()],
        )
        .unwrap();

        assert_eq!(command.get_program(), executable.as_os_str());
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                std::ffi::OsStr::new(COMMAND),
                std::ffi::OsStr::new("1234567890abcdef-service"),
                lease_path.as_os_str(),
                start_path.as_os_str(),
                std::ffi::OsStr::new("--"),
                std::ffi::OsStr::new("program"),
                std::ffi::OsStr::new("argument with spaces"),
            ]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn start_signal_is_atomic_idempotent_and_never_reassigned() {
        let root = std::env::temp_dir().join("gowild-supervisor-start-signal-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("start");
        let owner = "1234567890abcdef-owner";

        write_start_signal(&path, owner).unwrap();
        write_start_signal(&path, owner).unwrap();
        assert_eq!(
            write_start_signal(&path, "1234567890abcdef-contender")
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{owner}\n")
        );
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn supervisor_args_require_safe_sibling_control_paths_and_direct_argv() {
        let root = std::env::temp_dir().join("gowild-supervisor-argument-test");
        std::fs::create_dir_all(&root).unwrap();
        let parsed = parse_args(&[
            "1234567890abcdef".into(),
            root.join("lease.json").display().to_string(),
            root.join("start").display().to_string(),
            "--".into(),
            "program".into(),
            "argument with spaces".into(),
        ])
        .unwrap();
        assert_eq!(parsed.argv, ["program", "argument with spaces"]);
        assert!(parse_args(&[
            "1234567890abcdef".into(),
            root.join("lease.json").display().to_string(),
            root.join("other/start").display().to_string(),
            "--".into(),
            "program".into(),
        ])
        .is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lease_reader_rejects_unknown_fields_and_invalid_identity() {
        let root = std::env::temp_dir().join("gowild-supervisor-lease-test");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("lease.json");
        std::fs::write(
            &path,
            br#"{"version":1,"instance_id":"1234567890abcdef","pid":1,"started_at_unix_millis":1,"extra":true}"#,
        )
        .unwrap();
        assert_eq!(
            read_lease(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        std::fs::write(
            &path,
            br#"{"version":1,"instance_id":"1234567890abcdef","pid":0,"started_at_unix_millis":1}"#,
        )
        .unwrap();
        assert_eq!(
            read_lease(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lease_publication_is_atomic_and_never_clobbers_an_owner() {
        let root = std::env::temp_dir().join("gowild-supervisor-no-clobber-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("lease.json");
        let owner = ServiceSupervisorLease {
            version: LEASE_VERSION,
            instance_id: "1234567890abcdef-owner".into(),
            pid: 41,
            started_at_unix_millis: 42,
        };
        let contender = ServiceSupervisorLease {
            version: LEASE_VERSION,
            instance_id: "1234567890abcdef-contender".into(),
            pid: 51,
            started_at_unix_millis: 52,
        };

        write_lease(&path, &owner).unwrap();
        assert_eq!(
            write_lease(&path, &contender).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(read_lease(&path).unwrap(), owner);
        assert_eq!(
            std::fs::read_dir(&root).unwrap().count(),
            1,
            "failed publication must remove its temporary file"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
