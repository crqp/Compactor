use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use windows_sys::Win32::Foundation::ERROR_COMPRESSION_NOT_BENEFICIAL;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_PROVIDER_COMPRESSION_LZX, FILE_PROVIDER_COMPRESSION_XPRESS4K,
    FILE_PROVIDER_COMPRESSION_XPRESS8K, FILE_PROVIDER_COMPRESSION_XPRESS16K,
    GetCompressedFileSizeW, INVALID_FILE_SIZE, WOF_FILE_COMPRESSION_INFO_V1, WOF_PROVIDER_FILE,
    WofIsExternalFile,
};
#[cfg(test)]
use windows_sys::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW,
    WofGetDriverVersion,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    FILE_PROVIDER_CURRENT_VERSION, FILE_PROVIDER_EXTERNAL_INFO_V1, FSCTL_DELETE_EXTERNAL_BACKING,
    FSCTL_SET_EXTERNAL_BACKING, WOF_CURRENT_VERSION, WOF_EXTERNAL_INFO,
};
use windows_sys::core::{BOOL, HRESULT};

#[cfg(test)]
const VS_FIXEDFILEINFO_SIGNATURE: u32 = 0xFEEF_04BD;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Compression {
    Xpress4k,
    #[default]
    Xpress8k,
    Xpress16k,
    Lzx,
}

impl std::fmt::Display for Compression {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Xpress4k => write!(f, "XPRESS4K"),
            Self::Xpress8k => write!(f, "XPRESS8K"),
            Self::Xpress16k => write!(f, "XPRESS16K"),
            Self::Lzx => write!(f, "LZX"),
        }
    }
}

impl FromStr for Compression {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "XPRESS4K" => Ok(Self::Xpress4k),
            "XPRESS8K" => Ok(Self::Xpress8k),
            "XPRESS16K" => Ok(Self::Xpress16k),
            "LZX" => Ok(Self::Lzx),
            _ => Err(()),
        }
    }
}

impl TryFrom<u32> for Compression {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            FILE_PROVIDER_COMPRESSION_XPRESS4K => Ok(Self::Xpress4k),
            FILE_PROVIDER_COMPRESSION_XPRESS8K => Ok(Self::Xpress8k),
            FILE_PROVIDER_COMPRESSION_XPRESS16K => Ok(Self::Xpress16k),
            FILE_PROVIDER_COMPRESSION_LZX => Ok(Self::Lzx),
            _ => Err(()),
        }
    }
}

impl From<Compression> for u32 {
    fn from(compression: Compression) -> Self {
        match compression {
            Compression::Xpress4k => FILE_PROVIDER_COMPRESSION_XPRESS4K,
            Compression::Xpress8k => FILE_PROVIDER_COMPRESSION_XPRESS8K,
            Compression::Xpress16k => FILE_PROVIDER_COMPRESSION_XPRESS16K,
            Compression::Lzx => FILE_PROVIDER_COMPRESSION_LZX,
        }
    }
}

#[repr(C)]
struct SetBackingInfo {
    wof: WOF_EXTERNAL_INFO,
    file: FILE_PROVIDER_EXTERNAL_INFO_V1,
}

impl From<Compression> for SetBackingInfo {
    fn from(compression: Compression) -> Self {
        Self {
            wof: WOF_EXTERNAL_INFO {
                Version: WOF_CURRENT_VERSION,
                Provider: WOF_PROVIDER_FILE,
            },
            file: FILE_PROVIDER_EXTERNAL_INFO_V1 {
                Version: FILE_PROVIDER_CURRENT_VERSION,
                Algorithm: compression.into(),
                Flags: 0,
            },
        }
    }
}

fn to_wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

const fn succeeded(hr: HRESULT) -> bool {
    hr >= 0
}

const fn hresult_code(hr: HRESULT) -> i32 {
    hr & 0xFFFF
}

pub fn size_on_disk<P: AsRef<Path>>(path: P) -> io::Result<u64> {
    let path = to_wide(path.as_ref().as_os_str());
    let mut high: u32 = 0;

    let low = unsafe { GetCompressedFileSizeW(path.as_ptr(), &raw mut high) };

    if low == INVALID_FILE_SIZE {
        let err = io::Error::last_os_error();

        if err.raw_os_error() != Some(0) {
            return Err(err);
        }
    }

    Ok((u64::from(high) << 32) | u64::from(low))
}

#[cfg(test)]
pub fn system_supports_compression() -> io::Result<bool> {
    let dll = to_wide(OsStr::new("WofUtil.dll"));
    let path = to_wide(OsStr::new("\\"));
    let mut handle = 0;

    let len = unsafe { GetFileVersionInfoSizeW(dll.as_ptr(), &raw mut handle) };

    if len == 0 {
        return Err(io::Error::last_os_error());
    }

    let mut buf = vec![0u8; len as usize];

    let ret = unsafe { GetFileVersionInfoW(dll.as_ptr(), handle, len, buf.as_mut_ptr().cast()) };

    if ret == 0 {
        return Err(io::Error::last_os_error());
    }

    let mut pinfo: *mut std::ffi::c_void = std::ptr::null_mut();
    let mut pinfo_size = 0;

    let ret = unsafe {
        VerQueryValueW(
            buf.as_ptr().cast(),
            path.as_ptr(),
            &raw mut pinfo,
            &raw mut pinfo_size,
        )
    };

    if ret == 0 {
        return Err(io::Error::last_os_error());
    }

    assert!(pinfo_size as usize >= std::mem::size_of::<VS_FIXEDFILEINFO>());
    assert!(!pinfo.is_null());

    let pinfo = unsafe { &*(pinfo as *const VS_FIXEDFILEINFO) };
    assert_eq!(pinfo.dwSignature, VS_FIXEDFILEINFO_SIGNATURE);

    Ok((pinfo.dwFileVersionMS >> 16) & 0xFFFF >= 10)
}

#[cfg(test)]
pub fn file_supports_compression<P: AsRef<Path>>(path: P) -> io::Result<bool> {
    let file = std::fs::File::open(path)?;
    let mut version: u32 = 0;

    let ret =
        unsafe { WofGetDriverVersion(file.as_raw_handle(), WOF_PROVIDER_FILE, &raw mut version) };

    Ok(succeeded(ret) && version > 0)
}

pub fn detect_compression<P: AsRef<OsStr>>(path: P) -> io::Result<Option<Compression>> {
    let path = to_wide(path.as_ref());

    let mut is_external: BOOL = 0;
    let mut provider: u32 = 0;
    let mut file_info = WOF_FILE_COMPRESSION_INFO_V1::default();
    let mut len = u32::try_from(std::mem::size_of::<WOF_FILE_COMPRESSION_INFO_V1>())
        .map_err(io::Error::other)?;

    let ret = unsafe {
        WofIsExternalFile(
            path.as_ptr(),
            &raw mut is_external,
            &raw mut provider,
            std::ptr::addr_of_mut!(file_info).cast(),
            &raw mut len,
        )
    };

    if succeeded(ret) {
        if is_external != 0 && provider == WOF_PROVIDER_FILE {
            Ok(Compression::try_from(file_info.Algorithm).ok())
        } else {
            Ok(None)
        }
    } else {
        Err(io::Error::from_raw_os_error(hresult_code(ret)))
    }
}

pub fn compress_file_handle(file: &std::fs::File, compression: Compression) -> io::Result<bool> {
    let info = SetBackingInfo::from(compression);
    let input_size =
        u32::try_from(std::mem::size_of::<SetBackingInfo>()).map_err(io::Error::other)?;
    let mut bytes_returned: u32 = 0;

    let ret = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_SET_EXTERNAL_BACKING,
            std::ptr::addr_of!(info).cast(),
            input_size,
            std::ptr::null_mut(),
            0,
            &raw mut bytes_returned,
            std::ptr::null_mut(),
        )
    };

    if ret != 0 {
        Ok(true)
    } else {
        let err = io::Error::last_os_error();

        if err.raw_os_error() == Some(ERROR_COMPRESSION_NOT_BENEFICIAL.cast_signed()) {
            Ok(false)
        } else {
            Err(err)
        }
    }
}

pub fn uncompress_file_handle(file: &std::fs::File) -> io::Result<()> {
    let mut bytes_returned: u32 = 0;

    let ret = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_DELETE_EXTERNAL_BACKING,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &raw mut bytes_returned,
            std::ptr::null_mut(),
        )
    };

    if ret != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[test]
fn compact_works_i_guess() {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_READ_DATA, FILE_WRITE_ATTRIBUTES};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("compressible.txt");
    std::fs::write(&path, "hello world\n".repeat(10_000)).unwrap();

    if !system_supports_compression().expect("system_supports_compression") {
        return;
    }

    if !file_supports_compression(&path).expect("file_supports_compression") {
        return;
    }

    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES | FILE_READ_DATA)
        .open(&path)
        .unwrap();

    assert_eq!(None, detect_compression(&path).expect("detect_compression"));

    if compress_file_handle(&file, Compression::default()).expect("compress_file") {
        assert_eq!(
            Some(Compression::default()),
            detect_compression(&path).expect("detect_compression")
        );

        uncompress_file_handle(&file).expect("uncompress_file");
        assert_eq!(None, detect_compression(&path).expect("detect_compression"));
    }
}
