//! Transparent decompression dispatch by URL extension.

use std::io::Read;

use rpm_spec_repo_core::RepoError;

/// Upper bound on decompressed output. Large enough to hold the
/// biggest realistic Fedora-class `filelists.xml` (a couple of GB at
/// the time of writing) but small enough to keep a 1 KB hostile `.gz`
/// from blowing up the process. If hit, [`decompress`] returns
/// [`RepoError::Decompress`] with a "likely decompression bomb"
/// message rather than letting the allocator OOM.
pub const MAX_DECOMPRESSED_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Decompress `bytes` based on `name`'s suffix (`.gz`, `.zst`, `.xz`,
/// `.bz2`). Pass-through when no known suffix matches. All decoders
/// are wrapped in a [`MAX_DECOMPRESSED_BYTES`] limit; oversized
/// outputs error rather than allocate.
pub fn decompress(name: &str, bytes: &[u8]) -> Result<Vec<u8>, RepoError> {
    if name.ends_with(".gz") {
        return read_capped(name, flate2::read::GzDecoder::new(bytes));
    }
    if name.ends_with(".zst") {
        let dec = zstd::stream::read::Decoder::new(bytes)
            .map_err(|e| RepoError::Decompress(e.to_string()))?;
        return read_capped(name, dec);
    }
    if name.ends_with(".xz") {
        return read_capped(name, xz2::read::XzDecoder::new(bytes));
    }
    if name.ends_with(".bz2") {
        // bz2 is rare in modern rpm-md; the rare time we see it on a
        // legacy mirror, surface a clear "unsupported" error rather
        // than silently mis-parsing.
        return Err(RepoError::Decompress(format!(
            "bzip2 compression not enabled for {name}; please ask upstream to publish .gz or .zst"
        )));
    }
    Ok(bytes.to_vec())
}

/// Drain `inner` into a fresh `Vec`, capped at
/// [`MAX_DECOMPRESSED_BYTES`]. Shared body for the gz/zst/xz arms so
/// the bomb-check + error mapping stays in exactly one place.
fn read_capped<R: Read>(name: &str, inner: R) -> Result<Vec<u8>, RepoError> {
    let mut d = inner.take(MAX_DECOMPRESSED_BYTES);
    let mut out = Vec::new();
    d.read_to_end(&mut out)
        .map_err(|e| RepoError::Decompress(e.to_string()))?;
    check_bomb(name, out.len() as u64)?;
    Ok(out)
}

fn check_bomb(name: &str, len: u64) -> Result<(), RepoError> {
    if len >= MAX_DECOMPRESSED_BYTES {
        return Err(RepoError::Decompress(format!(
            "{name}: decompressed output exceeded {MAX_DECOMPRESSED_BYTES} byte cap (likely a decompression bomb)"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const PAYLOAD: &[u8] = b"hello world";

    #[test]
    fn roundtrip_gz() {
        let mut enc =
            flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(PAYLOAD).unwrap();
        let gzipped = enc.finish().unwrap();
        let out = decompress("x.gz", &gzipped).unwrap();
        assert_eq!(out, PAYLOAD);
    }

    #[test]
    fn roundtrip_zst() {
        let zst = zstd::stream::encode_all(PAYLOAD, 0).unwrap();
        let out = decompress("x.zst", &zst).unwrap();
        assert_eq!(out, PAYLOAD);
    }

    #[test]
    fn roundtrip_xz() {
        let mut enc = xz2::write::XzEncoder::new(Vec::new(), 6);
        enc.write_all(PAYLOAD).unwrap();
        let xz = enc.finish().unwrap();
        let out = decompress("x.xz", &xz).unwrap();
        assert_eq!(out, PAYLOAD);
    }

    #[test]
    fn bz2_unsupported_errors() {
        let err = decompress("x.bz2", b"whatever").unwrap_err();
        match err {
            RepoError::Decompress(msg) => {
                assert!(
                    msg.contains("bzip2 compression not enabled"),
                    "unexpected message: {msg}"
                );
            }
            other => panic!("expected Decompress error, got {other:?}"),
        }
    }

    #[test]
    fn passthrough_unknown_suffix() {
        let out = decompress("x.txt", b"plain").unwrap();
        assert_eq!(out, b"plain".to_vec());
    }
}
