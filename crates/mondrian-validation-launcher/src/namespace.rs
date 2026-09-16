//! Windows capsule namespace authority for same-user filesystem races.
//!
//! This does not claim resistance to code injection or privileged handle theft.
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SetSecurityInfo,
    SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    GetSecurityDescriptorDacl, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
};

// OWNER RIGHTS suppresses the owner's implicit WRITE_DAC grant. Everyone is
// denied namespace mutation, ownership and ACL writes; reads/execution remain.
// OI/CI propagate that exact policy to staged files: omitting inheritance would
// remove their prior inherited grants and deny fresh loader opens after sealing.
const SEALED_DACL: &str = "D:P(D;OICI;WDWO;;;OW)(D;OICI;0x000d0156;;;WD)(A;OICI;FRFX;;;WD)";

struct LocalAllocation(*mut std::ffi::c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}

#[derive(Debug)]
/// Retained pre-seal WRITE_DAC authority; only this owner restores the namespace.
pub struct CapsuleSeal {
    directory: File,
    original: String,
    sealed: String,
    restored: bool,
    children: Vec<ChildAclOwner>,
}

#[derive(Debug)]
struct ChildAclOwner {
    file: File,
    path: std::path::PathBuf,
    original: String,
    restored: bool,
}

impl CapsuleSeal {
    /// Seal the exact directory and retain the only restoration handle.
    pub fn seal(path: &Path) -> io::Result<Self> {
        let directory = OpenOptions::new()
            .access_mode(0x0002_0000 | 0x0004_0000) // READ_CONTROL | WRITE_DAC
            .share_mode(1)
            .custom_flags(0x0200_0000 | 0x0020_0000)
            .open(path)?;
        let original = read_dacl(&directory)?;
        let mut children = Vec::new();
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let metadata = std::fs::symlink_metadata(entry.path())?;
            if !metadata.is_file()
                || metadata.file_attributes() & 0x400 != 0
                || children.len() >= 1024
            {
                return Err(io::Error::other(
                    "capsule ACL inventory must contain only bounded direct files",
                ));
            }
            let file = OpenOptions::new()
                .access_mode(0x0002_0000 | 0x0004_0000)
                .share_mode(1)
                .custom_flags(0x0020_0000)
                .open(entry.path())?;
            let original = read_dacl(&file)?;
            children.push(ChildAclOwner {
                file,
                path: entry.path(),
                original,
                restored: false,
            });
        }
        let mut owner = Self {
            directory,
            original,
            sealed: String::new(),
            restored: false,
            children,
        };
        let sealed =
            set_dacl(&owner.directory, SEALED_DACL).and_then(|()| read_dacl(&owner.directory));
        match sealed {
            Ok(sealed) => {
                // Compare the OS-normalized descriptor, not hand-written SDDL spelling.
                let expected = canonical_dacl(SEALED_DACL)?;
                if !same_protected_policy(&sealed, &expected) {
                    let cleanup = owner.restore();
                    return Err(io::Error::other(format!(
                        "capsule DACL readback mismatch; restoration: {cleanup:?}"
                    )));
                }
                owner.sealed = sealed;
                Ok(owner)
            }
            Err(error) => {
                let cleanup = owner.restore();
                Err(io::Error::other(format!(
                    "capsule namespace seal failed: {error}; restoration: {cleanup:?}"
                )))
            }
        }
    }

    /// Read back the current protected DACL using the retained object handle.
    pub fn validate(&self) -> io::Result<()> {
        if self.restored || read_dacl(&self.directory)? != self.sealed {
            return Err(io::Error::other("capsule namespace DACL changed"));
        }
        Ok(())
    }

    /// Native normalized protected security descriptor.
    pub fn descriptor(&self) -> &str {
        &self.sealed
    }

    /// Fallibly restore the prior DACL through the retained authority.
    pub fn restore(&mut self) -> io::Result<()> {
        let mut failures = Vec::new();
        // Parent propagation cannot reacquire WRITE_DAC after inherited deny
        // policy. Restore every child through its pre-seal object authority.
        for child in &mut self.children {
            if !child.restored {
                match set_dacl(&child.file, &child.original) {
                    Ok(()) => child.restored = true,
                    Err(error) => {
                        failures.push(format!("restore child {}: {error}", child.path.display()))
                    }
                }
            }
        }
        if !self.restored {
            match set_dacl(&self.directory, &self.original) {
                Ok(()) => self.restored = true,
                Err(error) => failures.push(format!("restore capsule directory: {error}")),
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(io::Error::other(failures.join("; ")))
        }
    }
}

impl Drop for CapsuleSeal {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            tracing::error!(%error, "capsule namespace DACL fallback restoration failed");
        }
    }
}

fn read_dacl(file: &File) -> io::Result<String> {
    let mut descriptor = std::ptr::null_mut();
    let error = unsafe {
        GetSecurityInfo(
            file.as_raw_handle().cast(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    let allocation = LocalAllocation(descriptor);
    descriptor_string(allocation.0)
}

fn parse_dacl(text: &str) -> io::Result<LocalAllocation> {
    let wide = text.encode_utf16().chain(std::iter::once(0)).collect::<Vec<_>>();
    let mut descriptor = std::ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide.as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(LocalAllocation(descriptor))
}

fn canonical_dacl(text: &str) -> io::Result<String> {
    let descriptor = parse_dacl(text)?;
    descriptor_string(descriptor.0)
}

fn descriptor_string(descriptor: *mut std::ffi::c_void) -> io::Result<String> {
    let mut text = std::ptr::null_mut();
    let mut length = 0;
    if unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            1,
            DACL_SECURITY_INFORMATION,
            &mut text,
            &mut length,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let allocation = LocalAllocation(text.cast());
    if length == 0 || length > 65_536 {
        return Err(io::Error::other("capsule descriptor length invalid"));
    }
    let units =
        unsafe { std::slice::from_raw_parts(allocation.0.cast::<u16>(), length as usize - 1) };
    String::from_utf16(units).map_err(io::Error::other)
}

fn set_dacl(file: &File, text: &str) -> io::Result<()> {
    let descriptor = parse_dacl(text)?;
    let mut present = 0;
    let mut defaulted = 0;
    let mut dacl = std::ptr::null_mut();
    if unsafe { GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    if present == 0 || dacl.is_null() {
        return Err(io::Error::other("capsule descriptor has no explicit DACL"));
    }
    let error = unsafe {
        SetSecurityInfo(
            file.as_raw_handle().cast(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dacl,
            std::ptr::null_mut(),
        )
    };
    if error == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(error as i32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sealed_namespace_denies_new_files_and_owner_dacl_reopen_until_restoration() {
        let root = tempfile::tempdir().expect("capsule directory");
        std::fs::write(root.path().join("ffmpeg.exe"), b"retained").expect("fixture");
        let mut seal = CapsuleSeal::seal(root.path()).expect("seal namespace");
        seal.validate().expect("exact readback");
        // SetSecurityInfo propagates inheritable ACEs to existing children.
        // Loader/read opens must survive sealing, while fresh mutation fails.
        assert_eq!(
            std::fs::read(root.path().join("ffmpeg.exe")).expect("fresh image read after seal"),
            b"retained"
        );
        assert!(std::fs::write(root.path().join("ffmpeg.exe"), b"modified").is_err());
        assert!(std::fs::rename(
            root.path().join("ffmpeg.exe"),
            root.path().join("renamed.exe")
        )
        .is_err());
        assert!(std::fs::write(root.path().join("injected.dll"), b"bad").is_err());
        assert!(OpenOptions::new()
            .access_mode(0x0004_0000)
            .custom_flags(0x0200_0000)
            .open(root.path())
            .is_err());
        seal.restore().expect("retained handle restores DACL");
        drop(seal);
        std::fs::write(root.path().join("ffmpeg.exe"), b"restored child access")
            .expect("retained child ACL authority restores mutation access");
        std::fs::write(root.path().join("after-close.dll"), b"allowed")
            .expect("restored namespace");
        root.close().expect("explicit cleanup");
    }
}

/// Verify the exact protected policy through a fresh read-only native handle.
/// Offered SDDL is compared against this library's canonical policy, never trusted.
pub fn verify_sealed_namespace(path: &Path, offered_sddl: &str) -> io::Result<()> {
    let directory = OpenOptions::new()
        .access_mode(0x0002_0000)
        .share_mode(1)
        .custom_flags(0x0220_0000)
        .open(path)?;
    let expected = canonical_dacl(SEALED_DACL)?;
    if !same_protected_policy(offered_sddl, &expected) || read_dacl(&directory)? != offered_sddl {
        return Err(io::Error::other(
            "pre-loader namespace lacks the exact protected policy",
        ));
    }
    Ok(())
}

// GetSecurityInfo preserves SE_DACL_AUTO_INHERITED from the original directory
// (D:PAI) even after SetSecurityInfo applies the protected explicit ACL (D:P).
// AI is historical metadata. P and every ACE/right/flag remain exact; inherited
// ACE flags, absent protection, reordered entries, and extra grants still fail.
fn same_protected_policy(observed: &str, expected: &str) -> bool {
    observed.replacen("D:PAI(", "D:P(", 1) == expected
}

#[cfg(test)]
mod policy_readback_tests {
    use super::*;
    #[test]
    fn historical_auto_inheritance_does_not_remove_protected_policy() {
        let expected = canonical_dacl(SEALED_DACL).expect("native canonical policy");
        assert!(same_protected_policy(&expected, &expected));
        assert!(same_protected_policy(
            &expected.replacen("D:P(", "D:PAI(", 1),
            &expected
        ));
        assert!(!same_protected_policy(
            &expected.replacen("D:P(", "D:AI(", 1),
            &expected
        ));
        assert!(!same_protected_policy(
            &expected.replace("WDWO", "WD"),
            &expected
        ));
    }
}
