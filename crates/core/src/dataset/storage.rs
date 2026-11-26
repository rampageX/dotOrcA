use super::{invalid, MAX_BYTES};
use serde::Serialize;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::Path,
};

pub(super) fn read(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut data = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut data)?;
    if data.len() as u64 > MAX_BYTES {
        return Err(invalid("dataset too large"));
    }
    Ok(Some(data))
}

pub(super) fn lock(dir: &Path) -> io::Result<File> {
    fs::create_dir_all(dir)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("write.lock"))?;
    lock.lock()?;
    Ok(lock)
}

/// Caller holds the store lock. Interrupted writers leave only a staging
/// file. Readers use a single snapshot, and the next writer can reuse staging.
pub(super) fn replace(dir: &Path, name: &str, value: &impl Serialize) -> io::Result<()> {
    let data = serde_json::to_vec(value).map_err(io::Error::other)?;
    if data.len() as u64 > MAX_BYTES {
        return Err(invalid("dataset too large"));
    }
    let staged = dir.join(format!("{name}.pending"));
    let result = (|| {
        match fs::remove_file(&staged) {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)?;
        file.write_all(&data)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&staged, dir.join(name))?;
        #[cfg(unix)]
        File::open(dir)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(staged);
    }
    result
}
