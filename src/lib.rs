
use std::fs::File;
use std::io;
use std::os::unix::io::AsRawFd;

use anyhow::{Result, bail};
use linux_raw_sys::ioctl::FS_IOC_FIEMAP;
use rustix::io::Errno;

const FIEMAP_PAGE_SIZE: usize = 256;

#[repr(C)]
#[derive(Copy, Clone, Debug)]
struct FiemapExtent {
    fe_logical: u64,  // Logical offset in bytes for the start of the extent
    fe_physical: u64, // Physical offset in bytes for the start of the extent
    fe_length: u64,   // Length in bytes for the extent
    fe_reserved64: [u64; 2],
    fe_flags: u32, // FIEMAP_EXTENT_* flags for this extent
    fe_reserved: [u32; 3],
}
impl FiemapExtent {
    fn new() -> FiemapExtent {
        FiemapExtent {
            fe_logical: 0,
            fe_physical: 0,
            fe_length: 0,
            fe_reserved64: [0; 2],
            fe_flags: 0,
            fe_reserved: [0; 3],
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
struct FiemapReq {
    fm_start: u64,          // Logical offset (inclusive) at which to start mapping (in)
    fm_length: u64,         // Logical length of mapping which userspace cares about (in)
    fm_flags: u32,          // FIEMAP_FLAG_* flags for request (in/out)
    fm_mapped_extents: u32, // Number of extents that were mapped (out)
    fm_extent_count: u32,   // Size of fm_extents array (in)
    fm_reserved: u32,
    fm_extents: [FiemapExtent; FIEMAP_PAGE_SIZE], // Array of mapped extents (out)
}
impl FiemapReq {
    fn new() -> FiemapReq {
        FiemapReq {
            fm_start: 0,
            fm_length: u64::max_value(),
            fm_flags: 0,
            fm_mapped_extents: 0,
            fm_extent_count: FIEMAP_PAGE_SIZE as u32,
            fm_reserved: 0,
            fm_extents: [FiemapExtent::new(); FIEMAP_PAGE_SIZE],
        }
    }
}

#[allow(unused)]
fn quick_extents(fd: &File) -> Result<FiemapReq> {
    let req = FiemapReq::new();
    let req_ptr: *const FiemapReq = &req;

    if unsafe { libc::ioctl(fd.as_raw_fd(), FS_IOC_FIEMAP as u64, req_ptr) } != 0 {
        let oserr = io::Error::last_os_error();
        if oserr.raw_os_error() == Some(libc::EOPNOTSUPP) {
            bail!("Unuspported filesytem");
        }
        return Err(oserr.into());
    }
    Ok(req)
}


#[derive(PartialEq, Debug)]
enum SeekOff {
    Offset(u64),
    EOF,
}

#[allow(unused)]
fn lseek_to(fd: &File, to: u64) -> Result<SeekOff> {
    match rustix::fs::seek(fd, rustix::fs::SeekFrom::Start(to)) {
        Err(errno) if errno == Errno::NXIO => Ok(SeekOff::EOF),
        Err(err) => Err(err.into()),
        Ok(off) => Ok(SeekOff::Offset(off)),
    }
}


#[cfg(test)]
mod tests {
    use std::{env::current_dir, fs::{File, OpenOptions}, io::Write, process::Command, iter};

    use super::*;
    use tempfile::{tempdir_in, TempDir};

    fn tempdir() -> Result<TempDir> {
        // Force into local dir as /tmp might be tmpfs, which doesn't
        // support all VFS options (notably fiemap).
        Ok(tempdir_in(current_dir()?.join("target"))?)
    }

    #[test]
    fn test_extent_fetch() -> Result<()> {
        let dir = tempdir()?;
        let file = dir.path().join("sparse.bin");
        let from = dir.path().join("from.txt");
        let data = "test data";

        {
            let mut fd = File::create(&from)?;
            write!(fd, "{}", data)?;
        }

        let out = Command::new("/usr/bin/truncate")
            .args(["-s", "1M", file.to_str().unwrap()])
            .output()?;
        assert!(out.status.success());

        let offset = 512 * 1024;
        {
            let infd = File::open(&from)?;
            let outfd: File = OpenOptions::new().write(true).append(false).open(&file)?;
            let mut off_in = 0;
            let mut off_out = offset;
            let copied = rustix::fs::copy_file_range(
                &infd,
                Some(&mut off_in),
                &outfd,
                Some(&mut off_out),
                data.len(),
            )?;
            assert_eq!(copied as usize, data.len());
        }

        let fd = File::open(file)?;

        let extents = quick_extents(&fd)?;
        assert_eq!(extents.fm_mapped_extents, 1);

        Ok(())
    }

    #[test]
    fn test_extent_fetch_many() -> Result<()> {
        let dir = tempdir()?;
        let file = dir.path().join("sparse.bin");

        let out = Command::new("/usr/bin/truncate")
            .args(["-s", "1M", file.to_str().unwrap()])
            .output()?;
        assert!(out.status.success());

        let fsize = 1024 * 1024;
        // FIXME: Assumes 4k blocks
        let bsize = 4 * 1024;
        let block = iter::repeat(0xff_u8)
            .take(bsize)
            .collect::<Vec<u8>>();

        let mut fd = OpenOptions::new()
            .write(true)
            .append(false)
            .open(&file)?;
        // Skip every-other block
        for off in (0..fsize).step_by(bsize * 2) {
            lseek_to(&fd, off)?;
            fd.write_all(block.as_slice())?;
        }

        let extents = quick_extents(&fd)?;
        assert_eq!(extents.fm_mapped_extents, fsize as u32 / bsize as u32 / 2);

        Ok(())
    }

    #[test]
    fn test_extent_not_sparse() -> Result<()> {
        let dir = tempdir()?;
        let file = dir.path().join("file.bin");
        let size = 128 * 1024;

        {
            let mut fd: File = File::create(&file)?;
            let data = "X".repeat(size);
            write!(fd, "{}", data)?;
        }

        let fd = File::open(file)?;
        let extents = quick_extents(&fd)?;

        assert_eq!(1, extents.fm_mapped_extents);

        Ok(())
    }

}
