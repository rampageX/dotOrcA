use std::{io, os::windows::ffi::OsStrExt, path::Path};

#[link(name = "kernel32")]
unsafe extern "system" {
    fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
}

pub(super) fn publish(staged: &Path, destination: &Path) -> io::Result<()> {
    fn wide(path: &Path) -> io::Result<Vec<u16>> {
        let mut text: Vec<_> = path.as_os_str().encode_wide().collect();
        if text.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "NUL in output path",
            ));
        }
        text.push(0);
        Ok(text)
    }

    let staged = wide(staged)?;
    let destination = wide(destination)?;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    // Both buffers are live, NUL-terminated UTF-16 paths. The destination is
    // on the same volume and no replace/copy flags are supplied.
    if unsafe {
        MoveFileExW(
            staged.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
