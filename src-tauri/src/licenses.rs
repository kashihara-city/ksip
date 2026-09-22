use crate::message::{message};
pub fn text() -> Result<Vec<u8>, String> {
    miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(
        include_bytes!("../../temp/build/third-party-notices.zlib"),
        16 * 1024 * 1024,
    )
    .map_err(|_| message("LICENSE_TEXT_UNAVAILABLE"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn embedded_notices_restore_exactly() {
        // Read at test time so the uncompressed text is never embedded in the product.
        let original = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../temp/build/third-party-notices.txt"
        ))
        .unwrap();
        assert!(
            super::text().unwrap() == original,
            "Restored notices differ from source bytes"
        );
    }
}
