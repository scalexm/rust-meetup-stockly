use std::ffi::OsStr;
use std::fs::{self, File};
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::io::FromRawFd;
use std::path::{Path, PathBuf};

/// An anonymous temporary file, which removes itself on drop.
pub struct TmpFile {
    file: File,
    path: PathBuf,
}

impl TmpFile {
    pub fn create_in(directory: impl AsRef<Path>) -> io::Result<Self> {
        let template = directory.as_ref().join("XXXXXX");
        let (fd, path) = nix::unistd::mkstemp(&template)?;
        Ok(Self {
            file: unsafe { File::from_raw_fd(fd) },
            path,
        })
    }
}

impl io::Read for &TmpFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&self.file).read(buf)
    }
}

impl io::Write for &TmpFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&self.file).write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        (&self.file).flush()
    }
}

impl Drop for TmpFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
/// An `AtomicFile` emulates an on-disk `Atomic<Option<File>>`. Calling `load`
/// returns the latest known version of the file, and the file contents can be
/// updated by using the `compare_and_swap` operation.
///
/// Multiple `AtomicFile` pointing to the same path can be used simultaneously,
/// even across different threads or different processes on different machines:
/// it is guaranteed that there exists a total order across the updates made to
/// the file.
///
/// Internally, an `AtomicFile` is a directory containing successive versions
/// of a same logical file.
pub struct AtomicFile {
    directory: PathBuf,
    prefix: String,
}

fn parse_version(filename: &OsStr, prefix: &str) -> Option<usize> {
    let filename = filename.to_str()?;
    if !filename.starts_with(prefix) {
        return None;
    }
    filename[prefix.len()..].parse().ok()
}

impl AtomicFile {
    /// Open or create a directory located at `path` as an `AtomicFile`.
    /// The given `path` must specify the name of the directory: for example,
    /// `/` and `/path/to/..` would not be accepted.
    pub fn new(path: impl Into<PathBuf>) -> io::Result<Self> {
        let directory = path.into();
        fs::create_dir_all(&directory)?;
        let filename = match directory.file_name() {
            Some(filename) => filename,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "`path` must specify a directory name",
                ));
            }
        };
        let mut prefix = filename
            .to_str()
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "`path` must be valid UTF-8")
            })?
            .to_string();
        prefix.push('.');
        Ok(Self { directory, prefix })
    }

    fn latest_version(&self) -> io::Result<usize> {
        let mut max_version = 0;
        for entry in fs::read_dir(&self.directory)? {
            if let Some(version) = parse_version(&entry?.file_name(), &self.prefix) {
                max_version = std::cmp::max(max_version, version);
            }
        }
        Ok(max_version)
    }

    fn path(&self, version: usize) -> PathBuf {
        self.directory.join(format!("{}{version}", self.prefix))
    }

    /// Load the latest known version of the file.
    pub fn load(&self) -> io::Result<ReadOnlyFile> {
        let version = self.latest_version()?;
        let path = self.path(version);
        Ok(ReadOnlyFile { version, path })
    }

    /// Create a new temporary file, which can be written to.
    pub fn make_temp(&self) -> io::Result<TmpFile> {
        TmpFile::create_in(&self.directory)
    }

    /// Replace the contents of the file with the contents of `new` if the
    /// latest version is the same as `current`.
    ///
    /// # Errors
    /// If `io::ErrorKind::AlreadyExists` is returned, it means that the latest
    /// version was not the same as `current` and the operation must be retried
    /// with a fresher version of the file. Any other I/O error is forwarded as
    /// well.
    pub fn compare_and_swap(&self, current: &ReadOnlyFile, new: TmpFile) -> io::Result<()> {
        let new_path = self.path(current.version + 1);
        (&new.file).sync_data()?;
        // May return `EEXIST`.
        let res = nix::unistd::linkat(
            None,
            &new.path,
            None,
            &new_path,
            nix::unistd::LinkatFlags::NoSymlinkFollow,
        );
        if let Err(err) = res {
            // From open(2) manual page:
            //
            // "[...] create a unique file on the same filesystem (e.g.,
            // incorporating hostname and PID), and use link(2) to make a link
            // to the lockfile. If link(2) returns 0, the lock is successful.
            // Otherwise, use stat(2) on the unique file to check if its link
            // count has increased to 2, in which case the lock is also
            // succesful."
            if new.path.metadata()?.nlink() != 2 {
                Err(err)?;
            }
        }
        // Set read rights to everyone, don't care if that fails.
        let _ = fs::set_permissions(new_path, fs::Permissions::from_mode(0o644));
        Ok(())
    }
}

#[derive(Clone)]
pub struct ReadOnlyFile {
    version: usize,
    path: PathBuf,
}

impl ReadOnlyFile {
    /// Open the underlying file, which can be read from but not written to.
    /// May return `Ok(None)`, which means that no version of the `AtomicFile`
    /// has been created yet.
    pub fn open(&self) -> io::Result<Option<File>> {
        if self.version != 0 {
            Ok(Some(File::open(&self.path)?))
        } else {
            Ok(None)
        }
    }
}
