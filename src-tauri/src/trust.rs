//! The certificate authorities Windows trusts, in the form the engine reads.
//!
//! The engine's TLS library knows nothing of the Windows certificate store, so
//! the store is written out as PEM before each start. That file is the trust
//! anchor for a TLS connection whose settings name no authority of their own.
use crate::message::{message, message_with};
use windows_sys::Win32::Security::Cryptography::{
    CertCloseStore, CertEnumCertificatesInStore, CertOpenSystemStoreW, CertVerifyTimeValidity,
    CERT_CONTEXT, X509_ASN_ENCODING,
};

unsafe extern "C" {
    /// Whether the engine's TLS library can read this certificate. It reads a
    /// PEM bundle as all or nothing, so one it cannot read would empty the list.
    fn ksip_x509_parses(der: *const u8, length: std::ffi::c_long) -> i32;
}

/// The trust anchors taken from Windows, and how many were left out.
pub struct Trust {
    pub pem: String,
    pub included: usize,
    pub left_out: usize,
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
    let mut context: *const CERT_CONTEXT = std::ptr::null();
    loop {
        // SAFETY: each call frees the previous context and returns the next
        // one, or null at the end of the store.
        context = unsafe { CertEnumCertificatesInStore(handle, context) };
        if context.is_null() {
            break;
        }
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
        let der = unsafe { std::slice::from_raw_parts(encoded, length) };
        let in_date = unsafe { CertVerifyTimeValidity(std::ptr::null(), info) } == 0;
        let readable = unsafe { ksip_x509_parses(der.as_ptr(), length as std::ffi::c_long) } != 0;
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
    unsafe { CertCloseStore(handle, 0) };
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
    fn the_engine_library_refuses_what_is_not_a_certificate() {
        let junk = b"not a certificate";
        assert_eq!(unsafe { ksip_x509_parses(junk.as_ptr(), junk.len() as _) }, 0);
    }
}
