//! Files crossing the bridge. The server sends upload bytes as base64; the
//! extension gets a path to a private staged copy instead, because Chrome's
//! port takes at most 1 MB a message. Downloads go the other way: the
//! extension names the file Chrome saved and the bridge reads it, but only a
//! file the extension announced for a command the bridge relayed, and never
//! one under Haarness's own folders or a dot folder in the home folder, where
//! tokens and keys live.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Map, Value, json};
use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::{self, Read as _},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::time::Instant;
use uuid::Uuid;

/// The most one file may carry either way, as Desktop allows.
pub(crate) const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;
/// How long a staged upload outlives its command's result.
pub(crate) const KEEP_UPLOADS: Duration = Duration::from_secs(10 * 60);
/// Staging folders older than this belong to a bridge that did not stop cleanly.
const ABANDONED: Duration = Duration::from_secs(60 * 60);

/// A file name that is safe as one path component on every platform.
pub(crate) fn sanitize_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = base
        .chars()
        .filter(|character| !character.is_control())
        .map(|character| {
            if matches!(character, ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                character
            }
        })
        .collect();
    let mut cleaned = cleaned
        .trim()
        .trim_start_matches('.')
        .trim_end_matches(['.', ' '])
        .to_owned();
    if let Some((index, _)) = cleaned.char_indices().nth(120) {
        cleaned.truncate(index);
    }
    let stem = cleaned
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit());
    if cleaned.is_empty() {
        "upload".into()
    } else if reserved {
        format!("_{cleaned}")
    } else {
        cleaned
    }
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    fs::DirBuilder::new().mode(0o700).create(path)
}
#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir(path)
}

fn write_private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write as _;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Upload copies waiting for Chrome, per relayed command.
pub(crate) struct Uploads {
    root: PathBuf,
    pending: HashMap<(String, String), Vec<PathBuf>>,
    expiring: Vec<(Instant, PathBuf)>,
}

impl Uploads {
    /// Uploads under `root`, which is created private. Folders an earlier
    /// bridge left behind more than an hour ago are removed.
    pub(crate) fn new(root: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        }
        let root = super::discovery::plain(root.canonicalize()?);
        if let Ok(entries) = fs::read_dir(&root) {
            for entry in entries.flatten() {
                let old = entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .ok()
                    .and_then(|modified| modified.elapsed().ok())
                    .is_some_and(|age| age > ABANDONED);
                if old {
                    let _ = fs::remove_dir_all(entry.path());
                }
            }
        }
        Ok(Self {
            root,
            pending: HashMap::new(),
            expiring: Vec::new(),
        })
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Replaces every base64 `File` in `command.files` with a `FileRef` to a
    /// private copy under the uploads root.
    pub(crate) fn stage(
        &mut self,
        server: &str,
        id: &str,
        command: &mut Value,
    ) -> Result<(), String> {
        let Some(files) = command.get_mut("files").and_then(Value::as_array_mut) else {
            return Ok(());
        };
        for file in files.iter_mut() {
            let Some(data) = file.get("data").and_then(Value::as_str) else {
                continue;
            };
            let bytes = STANDARD
                .decode(data)
                .map_err(|_| "the file's data is not base64".to_owned())?;
            if bytes.len() as u64 > MAX_FILE_BYTES {
                return Err("the file is larger than 5 MiB".into());
            }
            let text = |key: &str| {
                file.get(key)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            let name = text("name");
            let folder = self.root.join(Uuid::now_v7().to_string());
            create_private_dir(&folder).map_err(|error| error.to_string())?;
            self.pending
                .entry((server.to_owned(), id.to_owned()))
                .or_default()
                .push(folder.clone());
            let path = folder.join(sanitize_name(&name));
            write_private_file(&path, &bytes).map_err(|error| error.to_string())?;
            *file = json!({
                "id": text("id"),
                "name": name,
                "mime": text("mime"),
                "bytes": bytes.len(),
                "path": path.display().to_string(),
            });
        }
        Ok(())
    }

    /// The command's result went back to the server: its copies go in ten minutes.
    pub(crate) fn finished(&mut self, server: &str, id: &str, now: Instant) {
        if let Some(folders) = self.pending.remove(&(server.to_owned(), id.to_owned())) {
            self.expiring.extend(
                folders
                    .into_iter()
                    .map(|folder| (now + KEEP_UPLOADS, folder)),
            );
        }
    }

    /// The server went away: no result will come for its commands.
    pub(crate) fn server_gone(&mut self, server: &str, now: Instant) {
        let ids: Vec<_> = self
            .pending
            .keys()
            .filter(|(owner, _)| owner == server)
            .map(|(_, id)| id.clone())
            .collect();
        for id in ids {
            self.finished(server, &id, now);
        }
    }

    pub(crate) fn sweep(&mut self, now: Instant) {
        self.expiring.retain(|(due, folder)| {
            if *due <= now {
                let _ = fs::remove_dir_all(folder);
                false
            } else {
                true
            }
        });
    }

    /// Removes every copy this bridge made, at shutdown.
    pub(crate) fn clear(&mut self) {
        for folder in self
            .pending
            .drain()
            .flat_map(|(_, folders)| folders)
            .chain(self.expiring.drain(..).map(|(_, folder)| folder))
        {
            let _ = fs::remove_dir_all(folder);
        }
    }
}

/// Folders a download may never come from.
pub(crate) struct Guard {
    /// Every Haarness data, config, cache and state folder (canonical).
    pub(crate) forbidden: Vec<PathBuf>,
    /// The home folder (canonical); its dot folders are off limits.
    pub(crate) home: Option<PathBuf>,
}

/// The bytes of a download Chrome saved at `path` since `since`.
pub(crate) fn read_download(
    path: &Path,
    since: SystemTime,
    guard: &Guard,
) -> Result<Vec<u8>, String> {
    if !path.is_absolute() {
        return Err("the path is not absolute".into());
    }
    let name = path.file_name().ok_or("the path names no file")?;
    let parent = path
        .parent()
        .ok_or("the path names no folder")?
        .canonicalize()
        .map(super::discovery::plain)
        .map_err(|_| "its folder does not exist".to_owned())?;
    let resolved = parent.join(name);
    if guard
        .forbidden
        .iter()
        .any(|root| resolved.starts_with(root))
    {
        return Err("it is inside Haarness's own folders".into());
    }
    if let Some(home) = &guard.home
        && let Ok(inside) = resolved.strip_prefix(home)
        && inside
            .components()
            .next()
            .is_some_and(|first| first.as_os_str().to_string_lossy().starts_with('.'))
    {
        return Err("it is inside a hidden folder".into());
    }
    let file = open_no_follow(&resolved).map_err(|error| {
        if is_symlink_refusal(&error) {
            "it is a symbolic link".to_owned()
        } else {
            "it cannot be opened".to_owned()
        }
    })?;
    let metadata = file
        .metadata()
        .map_err(|_| "it cannot be inspected".to_owned())?;
    if !metadata.is_file() {
        return Err("it is not a regular file".into());
    }
    if links(&metadata) != 1 {
        return Err("it has more than one name".into());
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err("it is larger than 5 MiB".into());
    }
    let floor = |time: SystemTime| {
        time.duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs())
    };
    if changed_at(&metadata).is_none_or(|changed| floor(changed) < floor(since)) {
        return Err("it is older than the command that downloaded it".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "it cannot be read".to_owned())?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err("it is larger than 5 MiB".into());
    }
    Ok(bytes)
}

#[cfg(unix)]
fn open_no_follow(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits() | nix::fcntl::OFlag::O_NONBLOCK.bits())
        .open(path)
}
#[cfg(not(unix))]
fn open_no_follow(path: &Path) -> io::Result<fs::File> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "symbolic link"));
    }
    fs::File::open(path)
}

fn is_symlink_refusal(error: &io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(nix::errno::Errno::ELOOP as i32)
    }
    #[cfg(not(unix))]
    {
        error.kind() == io::ErrorKind::InvalidInput
    }
}

#[cfg(unix)]
fn links(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    metadata.nlink()
}
#[cfg(not(unix))]
fn links(_metadata: &fs::Metadata) -> u64 {
    1
}

/// When the file last changed identity (ctime), or on Windows when it was created.
#[cfg(unix)]
fn changed_at(metadata: &fs::Metadata) -> Option<SystemTime> {
    use std::os::unix::fs::MetadataExt as _;
    let seconds = u64::try_from(metadata.ctime()).ok()?;
    let nanos = u32::try_from(metadata.ctime_nsec()).ok()?;
    Some(UNIX_EPOCH + Duration::new(seconds, nanos))
}
#[cfg(not(unix))]
fn changed_at(metadata: &fs::Metadata) -> Option<SystemTime> {
    metadata.created().ok()
}

/// A command the bridge relayed to the extension.
struct Relayed {
    thread: String,
    method: String,
    at: SystemTime,
}

/// A download the extension says Chrome finished for `command`.
struct Announced {
    server: String,
    thread: String,
    path: PathBuf,
    since: SystemTime,
}

/// Commands relayed and downloads announced, to decide which `files.get`
/// results may carry a file read from disk.
#[derive(Default)]
pub(crate) struct Downloads {
    relayed: HashMap<(String, String), Relayed>,
    order: VecDeque<(String, String)>,
    announced: VecDeque<Announced>,
}

const REMEMBERED_COMMANDS: usize = 4096;
const REMEMBERED_DOWNLOADS: usize = 512;

impl Downloads {
    pub(crate) fn relayed(
        &mut self,
        server: &str,
        id: &str,
        thread: &str,
        method: &str,
        at: SystemTime,
    ) {
        let key = (server.to_owned(), id.to_owned());
        if self
            .relayed
            .insert(
                key.clone(),
                Relayed {
                    thread: thread.to_owned(),
                    method: method.to_owned(),
                    at,
                },
            )
            .is_none()
        {
            self.order.push_back(key);
        }
        while self.order.len() > REMEMBERED_COMMANDS {
            if let Some(oldest) = self.order.pop_front() {
                self.relayed.remove(&oldest);
            }
        }
    }

    /// Records a finished download. Only a download during a command this
    /// bridge relayed to the same chat counts.
    pub(crate) fn announce(
        &mut self,
        server: &str,
        thread: &str,
        command: &str,
        path: &Path,
    ) -> bool {
        let Some(relayed) = self.relayed.get(&(server.to_owned(), command.to_owned())) else {
            return false;
        };
        if relayed.thread != thread || !path.is_absolute() {
            return false;
        }
        self.announced.retain(|announced| {
            !(announced.server == server && announced.thread == thread && announced.path == path)
        });
        self.announced.push_back(Announced {
            server: server.to_owned(),
            thread: thread.to_owned(),
            path: path.to_path_buf(),
            since: relayed.at,
        });
        while self.announced.len() > REMEMBERED_DOWNLOADS {
            self.announced.pop_front();
        }
        true
    }

    /// Turns the file references in a `result` for a relayed `files.get` into
    /// base64 files the server can store. Any other result that names a file
    /// on disk, or a file that may not be read, fails instead.
    pub(crate) fn resolve(&self, server: &str, message: &mut Value, guard: &Guard) {
        let id = message
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let Some(files) = message
            .pointer_mut("/outcome/result/files")
            .and_then(Value::as_array_mut)
        else {
            return;
        };
        let is_reference = |file: &Value| file.get("path").is_some() && file.get("data").is_none();
        if !files.iter().any(is_reference) {
            return;
        }
        let relayed = self
            .relayed
            .get(&(server.to_owned(), id))
            .filter(|relayed| relayed.method == "files.get");
        let converted = match relayed {
            None => Err("it was not asked for with files.get".to_owned()),
            Some(relayed) => files
                .iter()
                .map(|file| {
                    if !is_reference(file) {
                        return Ok(file.clone());
                    }
                    let path = Path::new(file["path"].as_str().ok_or("the path is not text")?);
                    let announced = self
                        .announced
                        .iter()
                        .rev()
                        .find(|announced| {
                            announced.server == server
                                && announced.thread == relayed.thread
                                && announced.path == path
                        })
                        .ok_or("Chrome did not report downloading it for this chat")?;
                    let bytes = read_download(path, announced.since, guard)?;
                    let mut converted = Map::new();
                    for key in ["id", "name", "mime"] {
                        converted.insert(key.into(), file.get(key).cloned().unwrap_or(Value::Null));
                    }
                    converted.insert("data".into(), STANDARD.encode(bytes).into());
                    Ok(Value::Object(converted))
                })
                .collect::<Result<Vec<_>, String>>(),
        };
        match converted {
            Ok(converted) => *files = converted,
            Err(reason) => {
                message["outcome"] = json!({
                    "type": "failure",
                    "code": "download-unreadable",
                    "message": format!("Chrome's download could not be read: {reason}"),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::discovery::tests::temp;

    #[test]
    fn upload_names_stay_one_harmless_path_component() {
        for (name, safe) in [
            ("report.pdf", "report.pdf"),
            ("../../etc/passwd", "passwd"),
            ("C:\\Users\\a\\secret.txt", "secret.txt"),
            (".bashrc", "bashrc"),
            ("..", "upload"),
            ("", "upload"),
            ("a:b*c?.txt", "a_b_c_.txt"),
            ("line\nbreak\u{7}.txt", "linebreak.txt"),
            ("trailing. . ", "trailing"),
            ("CON.txt", "_CON.txt"),
            ("com1", "_com1"),
            ("console.log", "console.log"),
            ("日本語.txt", "日本語.txt"),
        ] {
            assert_eq!(sanitize_name(name), safe, "{name:?}");
        }
        assert_eq!(sanitize_name(&"x".repeat(500)).chars().count(), 120);
    }

    #[tokio::test]
    async fn uploads_are_staged_privately_and_removed_after_their_result_or_at_shutdown() {
        let root = temp("uploads");
        let mut uploads = Uploads::new(root.join("uploads")).unwrap();
        let mut command = json!({"action":{"type":"files.upload"},"files":[
            {"id":"file_00000000-0000-0000-0000-000000000001","name":"../notes.txt","mime":"text/plain","data":STANDARD.encode("hello")},
            {"id":"file_00000000-0000-0000-0000-000000000002","name":"b.bin","mime":"application/octet-stream","data":""}
        ]});
        uploads.stage("server", "cmd-1", &mut command).unwrap();
        let reference = &command["files"][0];
        assert_eq!(reference["id"], "file_00000000-0000-0000-0000-000000000001");
        assert_eq!(reference["name"], "../notes.txt");
        assert_eq!(reference["mime"], "text/plain");
        assert_eq!(reference["bytes"], 5);
        assert!(reference.get("data").is_none());
        let path = PathBuf::from(reference["path"].as_str().unwrap());
        assert!(path.starts_with(uploads.root()));
        assert_eq!(path.file_name().unwrap(), "notes.txt");
        assert_eq!(fs::read(&path).unwrap(), b"hello");
        assert_eq!(command["files"][1]["bytes"], 0);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&path), 0o600);
            assert_eq!(mode(path.parent().unwrap()), 0o700);
            assert_eq!(mode(uploads.root()), 0o700);
        }

        // Kept for ten minutes after the result, then removed.
        let now = Instant::now();
        uploads.finished("server", "cmd-1", now);
        uploads.sweep(now + KEEP_UPLOADS - Duration::from_secs(1));
        assert!(path.exists());
        uploads.sweep(now + KEEP_UPLOADS);
        assert!(!path.exists());

        // Shutdown removes copies still waiting for a result.
        let mut command =
            json!({"files":[{"id":"f","name":"x","mime":"","data":STANDARD.encode("x")}]});
        uploads.stage("server", "cmd-2", &mut command).unwrap();
        let waiting = PathBuf::from(command["files"][0]["path"].as_str().unwrap());
        assert!(waiting.exists());
        uploads.clear();
        assert!(!waiting.exists());

        // Bad data fails the command instead of reaching Chrome.
        let mut command = json!({"files":[{"id":"f","name":"x","mime":"","data":"%%%"}]});
        assert!(uploads.stage("server", "cmd-3", &mut command).is_err());
        let mut command = json!({"files":[{"id":"f","name":"x","mime":"","data":STANDARD.encode(vec![0_u8; 5 * 1024 * 1024 + 1])}]});
        assert!(uploads.stage("server", "cmd-4", &mut command).is_err());
        uploads.clear();
        fs::remove_dir_all(root).unwrap();
    }

    fn guard(root: &Path) -> Guard {
        Guard {
            forbidden: vec![root.join("haarness-data")],
            home: Some(root.join("home")),
        }
    }

    #[test]
    fn downloads_are_read_only_from_ordinary_recent_files() {
        let root = temp("download-read");
        fs::create_dir_all(root.join("haarness-data")).unwrap();
        fs::create_dir_all(root.join("home/.ssh")).unwrap();
        fs::create_dir_all(root.join("home/Downloads")).unwrap();
        let guard = guard(&root);
        let before = SystemTime::now() - Duration::from_secs(5);
        let file = root.join("home/Downloads/invoice.pdf");
        fs::write(&file, b"%PDF").unwrap();
        assert_eq!(read_download(&file, before, &guard).unwrap(), b"%PDF");

        fs::write(root.join("haarness-data/runtime.json"), b"{}").unwrap();
        let refused = |path: &Path, since| read_download(path, since, &guard).unwrap_err();
        assert!(
            refused(&root.join("haarness-data/runtime.json"), before).contains("Haarness's own")
        );
        fs::write(root.join("home/.ssh/id_ed25519"), b"key").unwrap();
        assert!(refused(&root.join("home/.ssh/id_ed25519"), before).contains("hidden"));
        fs::write(root.join("home/.netrc"), b"x").unwrap();
        assert!(refused(&root.join("home/.netrc"), before).contains("hidden"));
        assert!(refused(Path::new("relative.pdf"), before).contains("absolute"));
        assert!(refused(&root.join("home/Downloads/missing.pdf"), before).contains("opened"));
        // Windows will not open a folder as a file at all.
        let folder = refused(&root.join("home/Downloads"), before);
        assert!(
            folder.contains("regular") || folder.contains("opened"),
            "{folder}"
        );
        // Older than the command.
        let later = SystemTime::now() + Duration::from_secs(5);
        assert!(refused(&file, later).contains("older"));
        // Too large.
        let large = root.join("home/Downloads/large.bin");
        fs::write(&large, vec![0_u8; 5 * 1024 * 1024 + 1]).unwrap();
        assert!(refused(&large, before).contains("5 MiB"));
        #[cfg(unix)]
        {
            // A link to a secret, as the file or as its folder.
            let link = root.join("home/Downloads/link.pdf");
            std::os::unix::fs::symlink(root.join("home/.ssh/id_ed25519"), &link).unwrap();
            assert!(refused(&link, before).contains("symbolic"));
            let folder = root.join("home/Downloads/folder");
            std::os::unix::fs::symlink(root.join("haarness-data"), &folder).unwrap();
            assert!(refused(&folder.join("runtime.json"), before).contains("Haarness's own"));
            // A second name for the same file.
            let hard = root.join("home/Downloads/hard.pdf");
            fs::hard_link(&file, &hard).unwrap();
            assert!(refused(&hard, before).contains("more than one name"));
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_files_get_result_carries_only_an_announced_download() {
        let root = temp("download-resolve");
        fs::create_dir_all(root.join("home/Downloads")).unwrap();
        fs::create_dir_all(root.join("haarness-data")).unwrap();
        let guard = guard(&root);
        let file = root.join("home/Downloads/report.csv");
        let mut downloads = Downloads::default();
        let thread = "019a0000-0000-7000-8000-000000000001";
        let relayed_at = SystemTime::now() - Duration::from_secs(2);
        downloads.relayed("server", "click-1", thread, "click", relayed_at);
        fs::write(&file, b"a,b\n").unwrap();
        downloads.relayed("server", "get-1", thread, "files.get", SystemTime::now());
        let result = |id: &str| {
            json!({"type":"result","id":id,"outcome":{"type":"success","result":{"value":{},"files":[
                {"id":"file_1","name":"report.csv","mime":"text/csv","bytes":4,"path":file.display().to_string()}
            ]}}})
        };
        // Not announced yet.
        let mut message = result("get-1");
        downloads.resolve("server", &mut message, &guard);
        assert_eq!(message["outcome"]["type"], "failure");
        assert!(
            message["outcome"]["message"]
                .as_str()
                .unwrap()
                .starts_with("Chrome's download could not be read: ")
        );

        // Announcements must name a relayed command of the same chat and server.
        assert!(!downloads.announce("server", thread, "unknown", &file));
        assert!(!downloads.announce(
            "server",
            "019a0000-0000-7000-8000-000000000002",
            "click-1",
            &file
        ));
        assert!(!downloads.announce("other", thread, "click-1", &file));
        assert!(downloads.announce("server", thread, "click-1", &file));
        let mut message = result("get-1");
        downloads.resolve("server", &mut message, &guard);
        assert_eq!(
            message["outcome"]["result"]["files"],
            json!([{"id":"file_1","name":"report.csv","mime":"text/csv","data":STANDARD.encode("a,b\n")}])
        );
        // The same reference in a result for anything but files.get fails.
        let mut message = result("click-1");
        downloads.resolve("server", &mut message, &guard);
        assert_eq!(message["outcome"]["code"], "download-unreadable");
        // Another server's announcement does not count.
        downloads.relayed("other", "get-2", thread, "files.get", SystemTime::now());
        let mut message = result("get-2");
        downloads.resolve("other", &mut message, &guard);
        assert_eq!(message["outcome"]["type"], "failure");
        // Results without file references pass untouched.
        let mut message = json!({"type":"result","id":"get-1","outcome":{"type":"success","result":{"value":1,"files":[{"id":"f","name":"a","mime":"b","data":"AA=="}]}}});
        let before = message.clone();
        downloads.resolve("server", &mut message, &guard);
        assert_eq!(message, before);
        fs::remove_dir_all(root).unwrap();
    }
}
