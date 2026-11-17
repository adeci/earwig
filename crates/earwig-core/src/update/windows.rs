//! Windows: the installer's own install of earwig updates by running the new
//! installer silently, which stops earwig, replaces it and starts it again;
//! a copy anywhere else (the portable zip, a build) only says a release is
//! out. A signed earwig runs only an installer its own publisher signed.

use std::ffi::{CStr, OsString};
use std::io;
use std::os::windows::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::ptr;

use sha2::{Digest as _, Sha256};
use windows_sys::Win32::Foundation::{
    CERT_E_REVOCATION_FAILURE, CRYPT_E_NO_REVOCATION_CHECK, CRYPT_E_REVOCATION_OFFLINE, HANDLE,
    INVALID_HANDLE_VALUE, TRUST_E_NOSIGNATURE, TRUST_E_PROVIDER_UNKNOWN,
    TRUST_E_SUBJECT_FORM_UNKNOWN,
};
use windows_sys::Win32::Security::Cryptography::{
    CERT_CONTEXT, CERT_FIND_EXT_ONLY_ENHKEY_USAGE_FLAG, CERT_NAME_SIMPLE_DISPLAY_TYPE, CTL_USAGE,
    CertGetEnhancedKeyUsage, CertGetNameStringW,
};
use windows_sys::Win32::Security::WinTrust::{
    WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO,
    WTD_CHOICE_FILE, WTD_REVOKE_WHOLECHAIN, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY,
    WTD_UI_NONE, WTHelperGetProvSignerFromChain, WTHelperProvDataFromStateData, WinVerifyTrust,
};
use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};

use super::publisher::{Publisher, Signature, judge};
use super::{Mode, UpdateError};
use crate::win32;

/// Where the installer (`AppId` in packaging/windows/earwig.iss) records the
/// install.
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{6A3C4F2E-8F1B-4C57-9E0D-2B7E5A1D9C41}_is1";

/// Set when packaging/windows/package.ps1 compiles a release package: only
/// those builds look for updates, not other cargo builds.
const RELEASE_PACKAGE: bool = option_env!("EARWIG_RELEASE_PACKAGE").is_some();

/// What a release package does: installs updates from the installer's own
/// install, says they are out elsewhere (the portable zip).
pub(super) fn detect() -> Option<Mode> {
    if !RELEASE_PACKAGE {
        return None;
    }
    let canonical = |dir: &Path| std::fs::canonicalize(dir).ok();
    let installed = win32::user_string(UNINSTALL_KEY, "InstallLocation")
        .and_then(|dir| canonical(Path::new(&dir)));
    let here = std::env::current_exe()
        .ok()
        .and_then(|exe| canonical(exe.parent()?));
    Some(if installed.is_some() && installed == here {
        Mode::Install
    } else {
        Mode::Notify
    })
}

/// Refuses `installer` unless this earwig may run it ([`judge`]); `page` has
/// it for an install by hand.
pub(super) fn check_publisher(installer: &Path, page: &str) -> Result<(), UpdateError> {
    let exe =
        std::env::current_exe().map_err(|e| UpdateError::io(e, "finding this program".into()))?;
    judge(signature(&exe), || signature(installer), page).map_err(UpdateError::Install)
}

/// What WinVerifyTrust makes of the signature of `path`, under Authenticode's
/// policy with revocation checked along the chain, as Windows checks a program
/// it downloaded.
fn signature(path: &Path) -> Signature {
    let path = win32::wide(path.as_os_str());
    let mut info = WINTRUST_FILE_INFO {
        cbStruct: u32::try_from(size_of::<WINTRUST_FILE_INFO>()).expect("small struct"),
        pcwszFilePath: path.as_ptr(),
        hFile: ptr::null_mut(),
        pgKnownSubject: ptr::null_mut(),
    };
    let mut data = WINTRUST_DATA {
        cbStruct: u32::try_from(size_of::<WINTRUST_DATA>()).expect("small struct"),
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_WHOLECHAIN,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 {
            pFile: &raw mut info,
        },
        dwStateAction: WTD_STATEACTION_VERIFY,
        ..Default::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    // SAFETY: `data` and what it points to outlive the call; with no window
    // there is no UI.
    let status = unsafe {
        WinVerifyTrust(
            INVALID_HANDLE_VALUE,
            &raw mut action,
            (&raw mut data).cast(),
        )
    };
    let error = io::Error::last_os_error().raw_os_error();
    let signature = match status {
        0 => signer(data.hWVTStateData).map_or_else(
            || Signature::Invalid("Windows found no signer in it".into()),
            Signature::Valid,
        ),
        status => failed(status, error),
    };
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    // SAFETY: frees what the verification kept in `data`.
    unsafe {
        WinVerifyTrust(
            INVALID_HANDLE_VALUE,
            &raw mut action,
            (&raw mut data).cast(),
        )
    };
    signature
}

/// What a verification that failed with `status`, the thread's last error
/// then `error`, says about the file.
fn failed(status: i32, error: Option<i32>) -> Signature {
    match status {
        // What Microsoft's example takes for no signature at all.
        TRUST_E_NOSIGNATURE
            if matches!(
                error,
                Some(TRUST_E_NOSIGNATURE | TRUST_E_SUBJECT_FORM_UNKNOWN | TRUST_E_PROVIDER_UNKNOWN)
            ) =>
        {
            Signature::Unsigned
        }
        // No word on revocation, as when the certificate authority's servers
        // are out of reach.
        CERT_E_REVOCATION_FAILURE | CRYPT_E_REVOCATION_OFFLINE | CRYPT_E_NO_REVOCATION_CHECK => {
            Signature::Unchecked
        }
        status => Signature::Invalid(describe(status)),
    }
}

/// Who signed the file whose verification left `state`, by the chain
/// WinVerifyTrust built from the signer's certificate to a trusted root.
fn signer(state: HANDLE) -> Option<Publisher> {
    // SAFETY: the state of a verification, alive until it is closed, as are
    // the provider data, signer, chain and certificates it leads to.
    unsafe {
        let provider = WTHelperProvDataFromStateData(state);
        if provider.is_null() {
            return None;
        }
        let chain = WTHelperGetProvSignerFromChain(provider, 0, 0, 0)
            .as_ref()?
            .pChainContext
            .as_ref()?;
        if chain.cChain == 0 || chain.rgpChain.is_null() {
            return None;
        }
        let simple = (*chain.rgpChain).as_ref()?;
        let count = usize::try_from(simple.cElement).ok()?;
        if count == 0 || simple.rgpElement.is_null() {
            return None;
        }
        let elements = std::slice::from_raw_parts(simple.rgpElement, count);
        let leaf = elements[0].as_ref()?.pCertContext.as_ref()?;
        let root = elements[count - 1].as_ref()?.pCertContext.as_ref()?;
        let encoded = std::slice::from_raw_parts(
            root.pbCertEncoded,
            usize::try_from(root.cbCertEncoded).ok()?,
        );
        Some(Publisher {
            name: name(leaf),
            ekus: ekus(leaf),
            root: Sha256::digest(encoded).into(),
            root_name: name(root),
        })
    }
}

/// The name Windows shows for `cert`: its subject's common name.
fn name(cert: &CERT_CONTEXT) -> String {
    let get = |text: *mut u16, len: u32| {
        // SAFETY: a valid certificate; `text` holds `len` characters, or is
        // null to ask for the length.
        unsafe {
            CertGetNameStringW(
                cert,
                CERT_NAME_SIMPLE_DISPLAY_TYPE,
                0,
                ptr::null(),
                text,
                len,
            )
        }
    };
    let len = get(ptr::null_mut(), 0);
    let mut text = vec![0u16; usize::try_from(len).unwrap_or(0)];
    let written = get(text.as_mut_ptr(), len);
    let end = usize::try_from(written.saturating_sub(1)).unwrap_or(0);
    String::from_utf16_lossy(&text[..end.min(text.len())])
}

/// The extended key usages `cert` names, none when it has no such extension.
fn ekus(cert: &CERT_CONTEXT) -> Vec<String> {
    let flags = CERT_FIND_EXT_ONLY_ENHKEY_USAGE_FLAG;
    let mut size = 0;
    // SAFETY: a valid certificate; asks for the size first, then fills a
    // buffer that large, aligned for the pointers in it, whose strings it
    // points into.
    unsafe {
        if CertGetEnhancedKeyUsage(cert, flags, ptr::null_mut(), &raw mut size) == 0 {
            return Vec::new();
        }
        let words = usize::try_from(size)
            .unwrap_or(0)
            .div_ceil(size_of::<usize>());
        let mut buffer = vec![0usize; words];
        if CertGetEnhancedKeyUsage(cert, flags, buffer.as_mut_ptr().cast(), &raw mut size) == 0 {
            return Vec::new();
        }
        let usage = &*buffer.as_ptr().cast::<CTL_USAGE>();
        let count = usize::try_from(usage.cUsageIdentifier).unwrap_or(0);
        if count == 0 || usage.rgpszUsageIdentifier.is_null() {
            return Vec::new();
        }
        std::slice::from_raw_parts(usage.rgpszUsageIdentifier, count)
            .iter()
            .map(|&oid| CStr::from_ptr(oid.cast()).to_string_lossy().into_owned())
            .collect()
    }
}

/// Windows' words for `status`, an HRESULT, with its number.
fn describe(status: i32) -> String {
    let text = io::Error::from_raw_os_error(status).to_string();
    let text = text.split(" (os error").next().unwrap_or_default();
    format!(
        "{} (0x{:08X})",
        text.trim_end_matches(['.', ' ']),
        status.cast_unsigned()
    )
}

/// Start `setup` silently in its own folder (the private download folder),
/// detached from earwig, which it stops (a silent upgrade leaves Start at
/// sign-in as it is); its log goes to `log`.
pub(super) fn launch(setup: &Path, log: &Path) -> io::Result<Child> {
    let mut log_arg = OsString::from("/LOG=");
    log_arg.push(log);
    let mut installer = Command::new(setup);
    if let Some(folder) = setup.parent() {
        installer.current_dir(folder);
    }
    installer
        .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"])
        .arg(log_arg)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()
}

#[cfg(test)]
mod tests {
    use windows_sys::Win32::Foundation::CERT_E_REVOKED;

    use super::*;

    #[test]
    fn no_word_on_revocation_is_unchecked_and_a_revoked_certificate_invalid() {
        for status in [
            CERT_E_REVOCATION_FAILURE,
            CRYPT_E_REVOCATION_OFFLINE,
            CRYPT_E_NO_REVOCATION_CHECK,
        ] {
            assert!(
                matches!(failed(status, None), Signature::Unchecked),
                "{status:#X}"
            );
        }
        assert!(matches!(
            failed(CERT_E_REVOKED, None),
            Signature::Invalid(_)
        ));
    }
}
