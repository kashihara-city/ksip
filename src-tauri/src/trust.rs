//! The certificate authorities Windows trusts, in the form the engine reads.
//!
//! The engine's TLS library knows nothing of the Windows certificate store, so
//! the store is written out as PEM before each start. That file is the trust
//! anchor for a TLS connection whose settings name no authority of their own.
use crate::message::{message, message_with};
use windows_sys::Win32::Security::Cryptography::{
    CertCloseStore, CertEnumCertificatesInStore, CertOpenSystemStoreW, CERT_CONTEXT,
    X509_ASN_ENCODING,
};

/// Every certificate in the trusted root and intermediate stores of the
/// current user, which include the ones installed for the whole machine.
pub fn windows_pem() -> Result<String, String> {
    let mut pem = String::new();
    let mut count = 0;
    for store in ["ROOT", "CA"] {
        let wide: Vec<u16> = store.encode_utf16().chain(Some(0)).collect();
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
            let (encoding, encoded, length) = unsafe {
                (
                    (*context).dwCertEncodingType,
                    (*context).pbCertEncoded,
                    (*context).cbCertEncoded as usize,
                )
            };
            if encoding & X509_ASN_ENCODING == 0 || encoded.is_null() || length == 0 {
                continue;
            }
            let der = unsafe { std::slice::from_raw_parts(encoded, length) };
            pem.push_str("-----BEGIN CERTIFICATE-----\n");
            let text = base64(der);
            for line in text.as_bytes().chunks(64) {
                pem.push_str(std::str::from_utf8(line).unwrap_or(""));
                pem.push('\n');
            }
            pem.push_str("-----END CERTIFICATE-----\n");
            count += 1;
        }
        unsafe { CertCloseStore(handle, 0) };
    }
    if count == 0 {
        return Err(message("TRUST_STORE_EMPTY"));
    }
    Ok(pem)
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
        let pem = windows_pem().unwrap();
        assert!(pem.starts_with("-----BEGIN CERTIFICATE-----\n"));
        assert!(pem.ends_with("-----END CERTIFICATE-----\n"));
        assert!(pem.lines().all(|line| line.len() <= 64 || line.starts_with("-----")));
    }
}
