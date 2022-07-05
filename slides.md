---
title:  File locking and hard to misuse APIs
author: Alexandre Martin
theme: Montpellier
---

[comment]: # Build with `pandoc -t beamer slides.md -o slides.pdf --pdf-engine=xelatex`

## Primitives provided by the OS

Assume an Unix-like OS. Goal is to synchronize concurrent accesses
(both read and write) to a file located on an NFS file server.

- POSIX advisory locks via `fcntl`
    - bound to a process rather than a file descriptor, makes it very hard to
      use it correctly especially in multi-threaded code
    - supported *in theory* by the NFS protocol, in practice implementations
      are often subtly broken
- BSD `flock`
    - nicer API, bound to file descriptors
    - simply does not work on an NFS

## Primitives provided by the OS

For an NFS file server, we need primitives that are atomic from the client
point of view.

- `open(path, O_EXCL)`
    - supported since NFS v3, has a good record of actually working in practice
    - can be used to create a "lock" file in an exclusive manner
- `link(path)`
    - manual page for `open(2)` explicitly hints at using `link` on NFS < v3
    - can emulate `open(path, O_EXCL)` by creating a unique temporary file and
      trying to use `link` to make a hard link to the lock file

Downside of using a lock file: need to poll on a regular basis + what if a
process dies holding it?

## Another protocol using `link`

- initial state

\tiny
```
-rw-r--r--  1 alexm  staff  7  3 jul 15:17 file.1
```
\normalsize

- a writer writes updated data in a temporary file

\tiny
```
-rw-r--r--  1 alexm  staff   7  3 jul 15:17 file.1
-rw-r--r--  1 alexm  staff  10  3 jul 15:21 file.tmp.357913
```
\normalsize

- the writer `link` it to what they think is the next version number, only one
  writer can succeed for the same version

\tiny
```
-rw-r--r--  1 alexm  staff   7  3 jul 15:17 file.1
-rw-r--r--  2 alexm  staff  10  3 jul 15:21 file.2
-rw-r--r--  2 alexm  staff  10  3 jul 15:21 file.tmp.357913
```
\normalsize

- (remove the temporary file)

\tiny
```
-rw-r--r--  1 alexm  staff   7  3 jul 15:17 file.1
-rw-r--r--  1 alexm  staff  10  3 jul 15:21 file.2
```
\normalsize

## Designing good abstractions around files

Key points when it comes to building abstractions:

- files and syscalls escape the type system entirely, a good abstraction
  needs to encode additional invariants in the type system with the least
  amount of dynamic pre-conditions
- files are difficult to grasp and end APIs should be about data rather than
  files

## Designing good abstractions around files

Files escape the type system:

- files can be aliased in any given way and accesses are unsynchronized by
  default
- Rust even embraces that by implementing IO traits on `&File`

Is it unsafe?

- in most cases no, the exception being `FromRawFd` but not for the reasons
  you think (see RFC 3128 on IO Safety)
- however it is easy to create bugs because of this

## Designing good abstractions around files

Use encapsulation to enforce invariants:

:::::::::::::: {.columns}
::: {.column width="50%"}
\tiny
```rust
/// An anonymous temporary file, which
/// removes itself on drop.
pub struct TmpFile {
    file: File,
    path: PathBuf,
}

impl TmpFile {
    pub fn create_in(
        directory: impl AsRef<Path>
    ) -> io::Result<Self> {
        let template = directory
            .as_ref()
            .join("XXXXXX");
        let (fd, path) = nix::unistd::mkstemp(
            &template
        )?;
        let file = unsafe {
            File::from_raw_fd(fd)
        };
        Ok(Self { file, path })
    }
}
```
\normalsize
:::
::: {.column width="50%"}
\tiny
```rust
impl io::Read for &TmpFile {
    fn read(
        &mut self, buf: &mut [u8]
    ) -> io::Result<usize> {
        (&self.file).read(buf)
    }
}

impl io::Write for &TmpFile {
    fn write(
        &mut self, buf: &[u8]
    ) -> io::Result<usize> {
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
```
\normalsize
:::
::::::::::::::

--------

Privacy of both `path` and `file` attributes of `TmpFile` allows us to enforce
the following invariants:

\tiny
```rust


fn link_to(tmp: TmpFile, path: impl AsRef<Path>) -> io::Result<()> {
    // Inside this function, nobody else can still have a handle to the file
    // located at `tmp.path` due to the following reasons:
    // * `replace_with` has the full ownership of `tmp` due to move semantics
    // * `TmpFile` does not disclose its path
    // * `TmpFile` does not disclose its file descriptor
    // * `TmpFile` does not implement `Clone`

    (&tmp.file).sync_data()?;
    
    // -> Consequence is that nobody can still be writing to the file located
    //    at `tmp.path`, and readers of the link located at `path` will never
    //    observe partial updates.
    nix::unistd::linkat(None, tmp.path, None, path, NoSymlinkFollow)?;
    Ok(())
}
```
\normalsize

## API of a solution to the introductory problem

The pattern "try to create the next version based on what we think the current
version is" is similar to how `compare_exchange` is used for normal memory.

\tiny
```rust


pub struct AtomicFile { ... }

pub struct ReadOnlyFile {
    version: usize,
    path: PathBuf,
}

impl ReadOnlyFile {
    /// May return `Ok(None)`, which means that no version of the `AtomicFile`
    /// has been created yet.
    pub fn open(&self) -> io::Result<Option<File>>;
}

impl AtomicFile {
    pub fn new(...) -> io::Result<Self>;
    pub fn load(&self) -> io::Result<ReadOnlyFile>;
    pub fn compare_and_swap(???self, current: ???ReadOnlyFile, new: ???TmpFile) -> io::Result<()>;
}
```
\normalsize

--------

\tiny
```rust
impl AtomicFile {
    pub fn compare_and_swap(&self, current: &ReadOnlyFile, new: TmpFile) -> io::Result<()> {
        let next_version = current.version + 1;
        let new_path = self.directory.join(format!("{}.{next_version}", self.prefix));
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
        Ok(())
    }
}
```
\normalsize

## Make it about data, not files

`AtomicFile` encapsulates the synchronization strategy but is still pretty
low-level and although being hard to misuse, it's not easy to use. A more
ergonomic API would allow direct modification of the file contents.


\tiny
```rust


pub fn modify(
    x: &AtomicFile,
    mut op: impl FnMut(&[u8]) -> Vec<u8>
) -> io::Result<()>;

pub fn modify_json<T: Serialize + DeserializeOwned>(
    x: &AtomicFile,
    mut op: impl FnMut(&mut Option<T>),
) -> io::Result<()>;
```
\normalsize

--------

\tiny
```rust
#[derive(serde::Deserialize, serde::Serialize, Default)]
struct Foo { count: i32 }

fn main() -> io::Result<()> {
    let file = Arc::new(AtomicFile::new("file.json")?);
    let mut handles = vec![];
    for _ in 0..2 {
        let file = file.clone();
        let h = std::thread::spawn(move || {
            modify_json(&file, |x: &mut Option<Foo>| match x {
                Some(x) => x.count += 1,
                None => *x = Some(Foo::default()),
            })
        });
        handles.push(h);
    }
    for h in handles {
        h.join().unwrap()?;
    }
    let mut data = String::new();
    file.load()?.open()?.unwrap().read_to_string(&mut data)?;
    assert!(serde_json::from_str::<Foo>(&data).unwrap().count % 2 == 1);
    Ok(())
}
```
\normalsize

## References

Full code and slides available at https://github.com/scalexm/rust-meetup-stockly .

* [On the Brokenness of File Locking](http://0pointer.net/blog/projects/locking.html)
* [SQLite code](https://www.sqlite.org/src/artifact/c230a7a24?ln=994-1081) --
"POSIX advisory locks are broken by design" + the amount of code trying to
circumvent *some* of the bugs encountered on NFS
* [SQLite FAQ](https://www.sqlite.org/faq.html) -- see question (5)
* [open(2) man page](https://man7.org/linux/man-pages/man2/open.2.html)
* [RFC 3128 IO Safety](https://rust-lang.github.io/rfcs/3128-io-safety.html)
