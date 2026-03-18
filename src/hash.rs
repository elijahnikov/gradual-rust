/// DJB-variant hash using 32-bit signed integer arithmetic.
/// Must produce identical results to the TypeScript implementation.
pub fn hash_string(s: &str) -> u32 {
    let mut hash: i32 = 0;
    for ch in s.chars() {
        let code = ch as i32;
        hash = hash.wrapping_shl(5).wrapping_sub(hash).wrapping_add(code);
    }
    hash.unsigned_abs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::fs;

    #[derive(Deserialize)]
    struct HashVector {
        input: String,
        hash: u32,
        bucket: u32,
    }

    #[derive(Deserialize)]
    struct HashVectorsFile {
        vectors: Vec<HashVector>,
    }

    fn load_vectors() -> Vec<HashVector> {
        let data = fs::read_to_string("../gradual-sdk-spec/testdata/hash/hash-vectors.json")
            .expect("Failed to load hash vectors");
        let file: HashVectorsFile = serde_json::from_str(&data).expect("Failed to parse");
        file.vectors
    }

    #[test]
    fn test_hash_conformance() {
        for v in load_vectors() {
            assert_eq!(
                hash_string(&v.input),
                v.hash,
                "hash_string({:?}) = {}, want {}",
                v.input,
                hash_string(&v.input),
                v.hash
            );
        }
    }

    #[test]
    fn test_bucket_conformance() {
        for v in load_vectors() {
            assert_eq!(
                hash_string(&v.input) % 100_000,
                v.bucket,
                "bucket({:?}) = {}, want {}",
                v.input,
                hash_string(&v.input) % 100_000,
                v.bucket
            );
        }
    }
}
