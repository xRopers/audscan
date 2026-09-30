//! Writing output files safely: to a temporary file next to the target, renamed into
//! place only once it has been written and checked, so a failure never leaves a
//! half-written or wrong file behind.

use std::path::{Path, PathBuf};

use crate::error::{Result, io_err};

/// Call `write` with a temporary path next to `path`, then rename it to `path` if
/// `write` succeeded. The temporary file is removed on any failure. `write` must close
/// every handle and mapping of the file before returning (Windows won't rename an
/// open or mapped file).
pub fn write_via_temp<T>(path: &Path, write: impl FnOnce(&Path) -> Result<T>) -> Result<T> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".audscan-tmp");
    let tmp = PathBuf::from(tmp);
    let result = write(&tmp).and_then(|value| std::fs::rename(&tmp, path).map(|()| value).map_err(io_err(path)));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;

    #[test]
    fn renames_on_success_and_cleans_up_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.bin");
        write_via_temp(&out, |tmp| std::fs::write(tmp, b"ok").map_err(io_err(tmp))).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"ok");

        let err = write_via_temp(&out, |tmp| {
            std::fs::write(tmp, b"bad").map_err(io_err(tmp))?;
            Err::<(), _>(Error::Pack("check failed".into()))
        });
        assert!(err.is_err());
        assert_eq!(std::fs::read(&out).unwrap(), b"ok", "the old file is kept");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "no temporary file left");
    }
}
