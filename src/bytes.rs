use atomic_file::{modify, AtomicFile};
use std::io::{self, Read};
use std::sync::Arc;

fn main() -> io::Result<()> {
    let file = Arc::new(AtomicFile::new("file")?);
    let mut handles = vec![];
    for i in 0..2 {
        let file = file.clone();
        let h = std::thread::spawn(move || {
            modify(&file, |data| {
                let mut data = data.to_vec();
                data.extend_from_slice(format!("thread{i}").as_bytes());
                data
            })
        });
        handles.push(h);
    }
    for h in handles {
        h.join().unwrap()?;
    }
    let mut data = String::new();
    file.load()?.open()?.unwrap().read_to_string(&mut data)?;
    assert!(data.ends_with("thread0thread1") || data.ends_with("thread1thread0"));
    Ok(())
}
