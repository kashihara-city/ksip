//! The certificate authorities Windows trusts, in the form the engine reads.
//!
//! The engine's TLS library knows nothing of the Windows certificate store, so
//! the store is written out as PEM before each start. That file is the trust
//! anchor for a TLS connection whose settings name no authority of their own.
//! Only roots are written, and only those Windows would trust for a TLS
//! server: what Windows distrusts is left out here, since the engine cannot
//! ask. An intermediate Windows distrusts is not caught this way (a server
//! sends its own chain), which is a limit of this approach.
use crate::message::{message, message_with};
use std::ffi::CStr;
use windows_sys::Win32::Foundation::{GetLastError, CRYPT_E_NOT_FOUND, FILETIME};
use windows_sys::Win32::Security::Cryptography::{
    CertCloseStore, CertEnumCertificatesInStore, CertFindCertificateInStore, CertFreeCertificateContext,
    CertGetCertificateContextProperty, CertGetEnhancedKeyUsage, CertOpenSystemStoreW, CertVerifyTimeValidity,
    CERT_CONTEXT, CERT_DISALLOWED_FILETIME_PROP_ID, CERT_FIND_EXISTING, CERT_FIND_PROP_ONLY_ENHKEY_USAGE_FLAG, CTL_USAGE,
    HCERTSTORE, X509_ASN_ENCODING,
};
use windows_sys::Win32::System::SystemInformation::GetSystemTimeAsFileTime;

// Declared as src-native/app/trust.cpp defines it:
// `int ksip_x509_parses(const unsigned char *der, long length)`.
unsafe extern "C" {
    /// Whether the engine's TLS library can read this certificate. It reads a
    /// PEM bundle as all or nothing, so one it cannot read would empty the list.
    fn ksip_x509_parses(der: *const u8, length: std::ffi::c_long) -> i32;
}

/// The trust anchors taken from Windows, and how many were left out: as
/// unusable (out of date, or unreadable by the engine), and as distrusted by
/// Windows (see `distrust`).
pub struct Trust {
    pub pem: String,
    pub included: usize,
    pub left_out: usize,
    pub distrusted: usize,
}

/// The purpose a PBX's certificate is used for: TLS server authentication.
const SERVER_AUTH: &CStr = c"1.3.6.1.5.5.7.3.1";

/// Why Windows would not trust this root for a TLS server, or None when it
/// would:
/// - it is in the Untrusted Certificates store (Disallowed), where an
///   administrator or Microsoft's automatic update puts what is distrusted;
/// - its purposes are restricted, by an administrator or by the root
///   program's certificate trust list, and TLS server authentication is not
///   among them (or no purpose is left at all);
/// - the date from which Windows no longer trusts what it issued has passed.
///   Windows would still accept a certificate issued before that date; the
///   engine cannot tell the dates apart, so the root is left out whole.
///
/// # Safety
/// `context` must be a live certificate context, and `disallowed` an open
/// certificate store or null.
unsafe fn distrust(context: *const CERT_CONTEXT, disallowed: HCERTSTORE) -> Option<&'static str> {
    if !disallowed.is_null() {
        // SAFETY: the store is open and the context live (the caller's
        // promise); a context found is freed at once, and only its being
        // there is used.
        let found = unsafe {
            CertFindCertificateInStore(disallowed, X509_ASN_ENCODING, 0, CERT_FIND_EXISTING, context.cast(), std::ptr::null())
        };
        if !found.is_null() {
            // SAFETY: `found` is the context the search returned, freed once, here.
            unsafe { CertFreeCertificateContext(found) };
            return Some("in Untrusted Certificates");
        }
    }
    // The purposes set on the certificate in the store (not those in the
    // certificate itself, which the engine reads for itself). No such
    // property means no restriction: the first call then fails with
    // CRYPT_E_NOT_FOUND.
    let mut size = 0u32;
    // SAFETY: a null buffer asks for the size only; `size` is a local.
    let sized = unsafe { CertGetEnhancedKeyUsage(context, CERT_FIND_PROP_ONLY_ENHKEY_USAGE_FLAG, std::ptr::null_mut(), &mut size) } != 0;
    if sized && size as usize >= std::mem::size_of::<CTL_USAGE>() {
        // Made of u64, so that the CTL_USAGE at its start (it holds a
        // pointer) is aligned; the identifiers it points to are in it too.
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        let usage = buffer.as_mut_ptr().cast::<CTL_USAGE>();
        // SAFETY: the buffer is aligned and `size` bytes long, as asked for
        // by the call before; the context is live.
        let read = unsafe { CertGetEnhancedKeyUsage(context, CERT_FIND_PROP_ONLY_ENHKEY_USAGE_FLAG, usage, &mut size) } != 0;
        if read {
            // SAFETY: the call filled the CTL_USAGE at the buffer's start.
            let usage = unsafe { &*usage };
            if usage.cUsageIdentifier == 0 {
                // Zero purposes read as "all" when the last error is
                // CRYPT_E_NOT_FOUND, and as "none" otherwise.
                // SAFETY: reads this thread's last error, which the call just set.
                if unsafe { GetLastError() } != CRYPT_E_NOT_FOUND as u32 {
                    return Some("allowed for no purpose");
                }
            } else {
                // SAFETY: the identifiers are `cUsageIdentifier` pointers to
                // null-terminated strings, all within the buffer, which lives
                // until the end of this block.
                let server = unsafe { std::slice::from_raw_parts(usage.rgpszUsageIdentifier, usage.cUsageIdentifier as usize) }
                    .iter()
                    // SAFETY: each non-null identifier is a null-terminated string in the buffer.
                    .any(|id| !id.is_null() && unsafe { CStr::from_ptr(id.cast()) } == SERVER_AUTH);
                if !server {
                    return Some("not allowed for TLS servers");
                }
            }
        }
    }
    let mut from = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let mut size = std::mem::size_of::<FILETIME>() as u32;
    // SAFETY: the buffer is a FILETIME of the size given; the context is live.
    let dated = unsafe {
        CertGetCertificateContextProperty(context, CERT_DISALLOWED_FILETIME_PROP_ID, (&mut from as *mut FILETIME).cast(), &mut size)
    } != 0;
    if dated && size as usize == std::mem::size_of::<FILETIME>() {
        let mut now = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        // SAFETY: fills the FILETIME given.
        unsafe { GetSystemTimeAsFileTime(&mut now) };
        let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
        if ticks(from) <= ticks(now) {
            return Some("distrusted from a date that has passed");
        }
    }
    None
}

/// Every certificate in the trusted root store of the current user, which
/// includes the ones installed for the whole machine. The intermediate store
/// is not an anchor on Windows either, and a server sends its own chain.
/// Certificates outside their validity period, and any the engine could not
/// read, are left out rather than spoiling the whole list.
pub fn windows_trust() -> Result<Trust, String> {
    let mut trust = Trust {
        pem: String::new(),
        included: 0,
        left_out: 0,
        distrusted: 0,
    };
    let wide: Vec<u16> = "ROOT".encode_utf16().chain(Some(0)).collect();
    // SAFETY: the name is null-terminated and the store handle is closed below.
    let handle = unsafe { CertOpenSystemStoreW(0, wide.as_ptr()) };
    if handle.is_null() {
        return Err(message_with(
            "TRUST_STORE_FAILED",
            [std::io::Error::last_os_error()],
        ));
    }
    // The Untrusted Certificates store, which is there on every Windows. One
    // that cannot be opened is not read as "nothing is distrusted": no roots
    // are handed over, and the start says why.
    let untrusted: Vec<u16> = "Disallowed".encode_utf16().chain(Some(0)).collect();
    // SAFETY: the name is null-terminated and the store handle is closed below.
    let disallowed = unsafe { CertOpenSystemStoreW(0, untrusted.as_ptr()) };
    if disallowed.is_null() {
        let error = std::io::Error::last_os_error();
        // SAFETY: the root store is open and closed once, here.
        unsafe { CertCloseStore(handle, 0) };
        return Err(message_with("TRUST_STORE_FAILED", [error]));
    }
    let mut context: *const CERT_CONTEXT = std::ptr::null();
    loop {
        // SAFETY: each call frees the previous context and returns the next
        // one, or null at the end of the store.
        context = unsafe { CertEnumCertificatesInStore(handle, context) };
        if context.is_null() {
            break;
        }
        // SAFETY: `context` is the non-null certificate the store just handed
        // out, alive until it is passed to the next enumeration.
        let (encoding, encoded, length, info) = unsafe {
            (
                (*context).dwCertEncodingType,
                (*context).pbCertEncoded,
                (*context).cbCertEncoded as usize,
                (*context).pCertInfo,
            )
        };
        if encoding & X509_ASN_ENCODING == 0 || encoded.is_null() || length == 0 {
            continue;
        }
        // SAFETY: the context is live until the next enumeration, and the
        // Untrusted Certificates store is open.
        if unsafe { distrust(context, disallowed) }.is_some() {
            trust.distrusted += 1;
            continue;
        }
        // SAFETY: the encoded certificate is `length` bytes (checked non-null
        // and non-empty above), owned by the context, which is alive until
        // the next enumeration; the slice is not kept past this iteration.
        let der = unsafe { std::slice::from_raw_parts(encoded, length) };
        // SAFETY: a null time means now; `info` is the context's own certificate info.
        let in_date = unsafe { CertVerifyTimeValidity(std::ptr::null(), info) } == 0;
        // The C function takes a long, 32 bits on Windows; cbCertEncoded is a
        // DWORD, so a length that does not fit is left out, not cut short.
        let Ok(long_length) = std::ffi::c_long::try_from(length) else {
            trust.left_out += 1;
            continue;
        };
        // SAFETY: the C function only reads `long_length` bytes from `der`,
        // which is exactly that long (checked to fit above), and does not keep it.
        let readable = unsafe { ksip_x509_parses(der.as_ptr(), long_length) } != 0;
        if !in_date || !readable {
            trust.left_out += 1;
            continue;
        }
        trust.pem.push_str("-----BEGIN CERTIFICATE-----\n");
        let text = base64(der);
        for line in text.as_bytes().chunks(64) {
            trust.pem.push_str(std::str::from_utf8(line).unwrap_or(""));
            trust.pem.push('\n');
        }
        trust.pem.push_str("-----END CERTIFICATE-----\n");
        trust.included += 1;
    }
    // SAFETY: both stores are open and closed once, here; the enumeration
    // ended with a null context, so no certificate context is still held.
    unsafe {
        CertCloseStore(handle, 0);
        CertCloseStore(disallowed, 0);
    }
    if trust.included == 0 {
        return Err(message("TRUST_STORE_EMPTY"));
    }
    Ok(trust)
}

/// Standard base64 with padding, which is all a PEM body needs.
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let second = chunk.get(1).copied().map_or(0, u32::from);
        let third = chunk.get(2).copied().map_or(0, u32::from);
        let n = (u32::from(chunk[0]) << 16) | (second << 8) | third;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"M"), "TQ==");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(&[0xfb, 0xff, 0xbf]), "+/+/");
    }

    #[test]
    fn the_windows_store_yields_pem_certificates() {
        let trust = windows_trust().unwrap();
        assert!(trust.included > 0);
        assert!(trust.pem.starts_with("-----BEGIN CERTIFICATE-----\n"));
        assert!(trust.pem.ends_with("-----END CERTIFICATE-----\n"));
        assert!(trust.pem.lines().all(|line| line.len() <= 64 || line.starts_with("-----")));
        // Every certificate handed over is one the engine's own library reads.
        let bundle = trust.pem.matches("-----BEGIN CERTIFICATE-----").count();
        assert_eq!(bundle, trust.included);
    }

    #[test]
    fn roots_windows_distrusts_are_found_and_others_are_not() {
        use windows_sys::core::PSTR;
        use windows_sys::Win32::Security::Cryptography::{
            CertAddCertificateContextToStore, CertOpenStore, CertSetCertificateContextProperty, CertSetEnhancedKeyUsage,
            CERT_STORE_ADD_ALWAYS, CERT_STORE_PROV_MEMORY, CRYPT_INTEGER_BLOB,
        };
        // Copies of a real root in stores of the test's own, in memory, so
        // that setting properties touches nothing of the machine's.
        // SAFETY: every store opened here is closed at the end and every
        // context taken is freed; the properties are set on the test's copy
        // only; the identifier strings and blobs outlive the calls that read them.
        unsafe {
            let memory = || CertOpenStore(CERT_STORE_PROV_MEMORY, 0, 0, 0, std::ptr::null());
            let (empty, scratch, untrusted) = (memory(), memory(), memory());
            assert!(!empty.is_null() && !scratch.is_null() && !untrusted.is_null());
            let name: Vec<u16> = "ROOT".encode_utf16().chain(Some(0)).collect();
            let root = CertOpenSystemStoreW(0, name.as_ptr());
            assert!(!root.is_null());
            // A root with nothing against it, for a clean start.
            let mut original = std::ptr::null();
            loop {
                original = CertEnumCertificatesInStore(root, original);
                assert!(!original.is_null(), "the root store has a root Windows trusts");
                if distrust(original, empty).is_none() {
                    break;
                }
            }
            let mut copy = std::ptr::null_mut();
            assert!(CertAddCertificateContextToStore(scratch, original, CERT_STORE_ADD_ALWAYS, &mut copy) != 0);
            assert_eq!(distrust(copy, empty), None, "a root with nothing against it is taken");

            // In Untrusted Certificates.
            assert!(CertAddCertificateContextToStore(untrusted, original, CERT_STORE_ADD_ALWAYS, std::ptr::null_mut()) != 0);
            assert!(distrust(copy, untrusted).is_some(), "a root that is also in Untrusted Certificates is left out");

            // Purposes restricted, without and with TLS server authentication,
            // and none at all.
            let client = c"1.3.6.1.5.5.7.3.2";
            let mut ids: [PSTR; 2] = [client.as_ptr() as PSTR, SERVER_AUTH.as_ptr() as PSTR];
            let only_client = CTL_USAGE { cUsageIdentifier: 1, rgpszUsageIdentifier: ids.as_mut_ptr() };
            assert!(CertSetEnhancedKeyUsage(copy, &only_client) != 0);
            assert!(distrust(copy, empty).is_some(), "a root restricted to other purposes is left out");
            let with_server = CTL_USAGE { cUsageIdentifier: 2, rgpszUsageIdentifier: ids.as_mut_ptr() };
            assert!(CertSetEnhancedKeyUsage(copy, &with_server) != 0);
            assert_eq!(distrust(copy, empty), None, "a root restricted to purposes that include TLS servers is taken");
            let none = CTL_USAGE { cUsageIdentifier: 0, rgpszUsageIdentifier: std::ptr::null_mut() };
            assert!(CertSetEnhancedKeyUsage(copy, &none) != 0);
            assert!(distrust(copy, empty).is_some(), "a root allowed for no purpose is left out");
            assert!(CertSetEnhancedKeyUsage(copy, std::ptr::null()) != 0);
            assert_eq!(distrust(copy, empty), None, "the restriction taken off, the root is taken again");

            // Distrusted from a date: past, and still to come.
            let date = |days_from_now: i64| {
                let mut now = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
                GetSystemTimeAsFileTime(&mut now);
                let ticks = ((u64::from(now.dwHighDateTime) << 32) | u64::from(now.dwLowDateTime)) as i64 + days_from_now * 864_000_000_000;
                let mut bytes = (ticks as u64).to_le_bytes();
                let blob = CRYPT_INTEGER_BLOB { cbData: 8, pbData: bytes.as_mut_ptr() };
                assert!(CertSetCertificateContextProperty(copy, CERT_DISALLOWED_FILETIME_PROP_ID, 0, (&blob as *const CRYPT_INTEGER_BLOB).cast()) != 0);
            };
            date(-1);
            assert!(distrust(copy, empty).is_some(), "a root distrusted from yesterday is left out");
            date(30);
            assert_eq!(distrust(copy, empty), None, "a root distrusted only from next month is still taken");

            CertFreeCertificateContext(copy);
            CertFreeCertificateContext(original);
            for store in [empty, scratch, untrusted, root] {
                CertCloseStore(store, 0);
            }
        }
    }

    #[test]
    fn the_engine_library_refuses_what_is_not_a_certificate() {
        let junk = b"not a certificate";
        // SAFETY: the C function reads the given bytes only.
        assert_eq!(unsafe { ksip_x509_parses(junk.as_ptr(), junk.len() as _) }, 0);
    }
}
