use atomic_file::{modify_json, AtomicFile};
use std::io::{self, Read};
use std::sync::Arc;

#[derive(serde::Deserialize, serde::Serialize, Default)]
struct Foo {
    count: i32,
}

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
