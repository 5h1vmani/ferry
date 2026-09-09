//! Chunking and integrity.
//!
//! A transfer is split into fixed-size chunks. Each chunk carries a hash, so a
//! receiver can verify one chunk without holding the whole file. The chunk
//! hashes also combine into a single hash of the whole file.
//!
//! # Why the hashes are not plain BLAKE3 hashes
//!
//! BLAKE3 builds a binary tree over the input. The hash of a whole file is the
//! root of that tree. Each internal node has a *chaining value*, which is the
//! same 32 bytes with one flag left unset.
//!
//! [`blake3::hash`] of a chunk is a root hash of that chunk alone. Root hashes
//! do not combine. Chaining values do. So the per-chunk values stored here are
//! chaining values, taken through the crate's `hazmat` interface.
//!
//! This is the reason the design can hash a file once and get both the
//! per-chunk values and the whole-file hash. Computing the chunk hashes the
//! obvious way would break that, and the failure would be silent.
//!
//! # Why the chunk size is a power of two
//!
//! A chaining value only means something for a complete subtree of the BLAKE3
//! tree. A subtree must start at an offset that is a multiple of its own
//! length, and its length must be a power of two multiple of
//! [`blake3::CHUNK_LEN`], which is 1024 bytes.
//!
//! [`ChunkSize`] enforces that. It is the only way to build a [`Manifest`].

use blake3::hazmat::{HasherExt, Mode, merge_subtrees_non_root, merge_subtrees_root};
use blake3::{CHUNK_LEN, Hash, Hasher};

/// A per-chunk chaining value. Thirty-two bytes.
///
/// This is not a hash of the chunk on its own. See the module documentation.
pub type ChainingValue = [u8; 32];

/// The reason a chunk size was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkSizeError {
    /// The size was not a power of two.
    NotPowerOfTwo,
    /// The size was below [`blake3::CHUNK_LEN`], which is 1024 bytes.
    TooSmall,
    /// The size was above [`ChunkSize::MAX`].
    TooLarge,
}

impl core::fmt::Display for ChunkSizeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::NotPowerOfTwo => "chunk size is not a power of two",
            Self::TooSmall => "chunk size is below 1024 bytes",
            Self::TooLarge => "chunk size is above the maximum",
        })
    }
}

impl std::error::Error for ChunkSizeError {}

/// A chunk size that BLAKE3 accepts as a subtree length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChunkSize(u32);

impl ChunkSize {
    /// The largest chunk size Ferry allows, in bytes.
    ///
    /// This bounds how much a receiver must buffer for one chunk.
    pub const MAX: u32 = 16 * 1024 * 1024;

    /// The default chunk size, one mebibyte.
    #[must_use]
    pub fn one_mebibyte() -> Self {
        Self(1024 * 1024)
    }

    /// Check a chunk size.
    ///
    /// # Errors
    ///
    /// Returns the rule the value broke. See [`ChunkSizeError`].
    pub fn new(bytes: u32) -> Result<Self, ChunkSizeError> {
        if !bytes.is_power_of_two() {
            return Err(ChunkSizeError::NotPowerOfTwo);
        }
        if (bytes as usize) < CHUNK_LEN {
            return Err(ChunkSizeError::TooSmall);
        }
        if bytes > Self::MAX {
            return Err(ChunkSizeError::TooLarge);
        }
        Ok(Self(bytes))
    }

    /// The size in bytes.
    #[must_use]
    pub fn get(self) -> u32 {
        self.0
    }

    /// The size in bytes, widened for offset arithmetic.
    #[must_use]
    pub fn as_u64(self) -> u64 {
        u64::from(self.0)
    }

    /// The size in bytes, as a slice length.
    #[must_use]
    pub fn as_usize(self) -> usize {
        self.0 as usize
    }
}

/// Everything needed to verify and resume one file transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    length: u64,
    chunk_size: ChunkSize,
    chunks: Vec<ChainingValue>,
    root: Hash,
}

impl Manifest {
    /// The file length in bytes.
    #[must_use]
    pub fn length(&self) -> u64 {
        self.length
    }

    /// The chunk size this manifest was built with.
    #[must_use]
    pub fn chunk_size(&self) -> ChunkSize {
        self.chunk_size
    }

    /// The number of chunks. Zero only for an empty file.
    #[must_use]
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// The chaining values, in order.
    #[must_use]
    pub fn chunks(&self) -> &[ChainingValue] {
        &self.chunks
    }

    /// The hash of the whole file.
    ///
    /// This equals `blake3::hash` of the same bytes.
    #[must_use]
    pub fn root(&self) -> Hash {
        self.root
    }

    /// The byte range covered by one chunk.
    ///
    /// Returns `None` when `index` is past the end.
    #[must_use]
    pub fn chunk_range(&self, index: usize) -> Option<(u64, u32)> {
        if index >= self.chunks.len() {
            return None;
        }
        let start = index as u64 * self.chunk_size.as_u64();
        let remaining = self.length - start;
        let len = remaining.min(self.chunk_size.as_u64());
        Some((start, u32::try_from(len).unwrap_or(self.chunk_size.get())))
    }

    /// Check that `bytes` really is chunk `index` of this file.
    ///
    /// This is how a receiver decides which chunks it already holds. It is also
    /// how it rejects a manifest that claims a chunk is present when the bytes
    /// on disk say otherwise.
    #[must_use]
    pub fn verify_chunk(&self, index: usize, bytes: &[u8]) -> bool {
        let Some((start, len)) = self.chunk_range(index) else {
            return false;
        };
        if bytes.len() as u64 != u64::from(len) {
            return false;
        }
        let actual = chunk_chaining_value(start, bytes);
        self.chunks[index] == actual
    }
}

/// Compute the chaining value of one chunk.
///
/// `offset` is where the chunk starts in the whole file. It must be a multiple
/// of the chunk size, or the value will be wrong.
#[must_use]
fn chunk_chaining_value(offset: u64, bytes: &[u8]) -> ChainingValue {
    Hasher::new()
        .set_input_offset(offset)
        .update(bytes)
        .finalize_non_root()
}

/// Build a manifest from a whole file held in memory.
///
/// This exists for tests and for small files. Large files use
/// [`ManifestBuilder`], which never holds more than one chunk.
#[must_use]
pub fn manifest_from_bytes(bytes: &[u8], chunk_size: ChunkSize) -> Manifest {
    let mut builder = ManifestBuilder::new(chunk_size);
    for piece in bytes.chunks(chunk_size.as_usize()) {
        builder.push(piece);
    }
    builder.finish()
}

/// Builds a [`Manifest`] one chunk at a time.
///
/// The caller feeds chunks in order. Every chunk except the last must be
/// exactly [`ChunkSize`] bytes long.
#[derive(Debug)]
pub struct ManifestBuilder {
    chunk_size: ChunkSize,
    chunks: Vec<ChainingValue>,
    length: u64,
    first_chunk_root: Option<Hash>,
}

impl ManifestBuilder {
    /// Start a builder for the given chunk size.
    #[must_use]
    pub fn new(chunk_size: ChunkSize) -> Self {
        Self {
            chunk_size,
            chunks: Vec::new(),
            length: 0,
            first_chunk_root: None,
        }
    }

    /// Add the next chunk.
    ///
    /// One pass over the bytes yields the chaining value. For a file that turns
    /// out to hold a single chunk, the same pass also yields the root hash,
    /// because a lone chunk is the whole tree.
    pub fn push(&mut self, bytes: &[u8]) {
        let hasher = {
            let mut h = Hasher::new();
            h.set_input_offset(self.length);
            h.update(bytes);
            h
        };
        if self.chunks.is_empty() {
            self.first_chunk_root = Some(hasher.finalize());
        }
        self.chunks.push(hasher.finalize_non_root());
        self.length += bytes.len() as u64;
    }

    /// Finish, producing the manifest.
    #[must_use]
    pub fn finish(self) -> Manifest {
        let root = match self.chunks.len() {
            // An empty file has no chunks, so its hash comes from no input.
            0 => blake3::hash(&[]),
            // A single chunk is the whole tree, so its root came from the same
            // pass that produced its chaining value.
            1 => self.first_chunk_root.unwrap_or_else(|| blake3::hash(&[])),
            _ => {
                let cv = merge_range(&self.chunks, 0, self.length, self.chunk_size, true);
                Hash::from_bytes(cv)
            }
        };
        Manifest {
            length: self.length,
            chunk_size: self.chunk_size,
            chunks: self.chunks,
            root,
        }
    }
}

/// Rebuild one node of the BLAKE3 tree from the chunk chaining values below it.
///
/// `start` and `len` describe the byte range this node covers. The range always
/// begins on a chunk boundary, because the recursion only ever splits on power
/// of two boundaries.
fn merge_range(
    chunks: &[ChainingValue],
    start: u64,
    len: u64,
    chunk_size: ChunkSize,
    is_root: bool,
) -> ChainingValue {
    debug_assert!(len > 0, "a tree node covers at least one byte");
    if len <= chunk_size.as_u64() {
        let index = usize::try_from(start / chunk_size.as_u64()).expect("chunk index fits");
        return chunks[index];
    }
    let left_len = blake3::hazmat::left_subtree_len(len);
    let left = merge_range(chunks, start, left_len, chunk_size, false);
    let right = merge_range(chunks, start + left_len, len - left_len, chunk_size, false);
    if is_root {
        merge_subtrees_root(&left, &right, Mode::Hash).into()
    } else {
        merge_subtrees_non_root(&left, &right, Mode::Hash)
    }
}

#[cfg(test)]
mod tests {
    use super::{ChunkSize, ChunkSizeError, ManifestBuilder, manifest_from_bytes};

    fn data(len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| u8::try_from(i % 251).unwrap_or(0))
            .collect()
    }

    #[test]
    fn chunk_size_rejects_values_blake3_cannot_use() {
        assert_eq!(ChunkSize::new(1000), Err(ChunkSizeError::NotPowerOfTwo));
        assert_eq!(ChunkSize::new(512), Err(ChunkSizeError::TooSmall));
        assert_eq!(ChunkSize::new(1 << 30), Err(ChunkSizeError::TooLarge));
        assert_eq!(ChunkSize::new(1024).unwrap().get(), 1024);
    }

    #[test]
    fn the_merged_root_equals_a_plain_blake3_hash() {
        // This is the claim the whole integrity design rests on. One pass over
        // the file yields per-chunk values, and those values merge back into
        // exactly what blake3::hash would have produced.
        let chunk_size = ChunkSize::new(1024).unwrap();
        for len in [
            0, 1, 1023, 1024, 1025, 2048, 3000, 4096, 5000, 8192, 8193, 100_000, 1_000_000,
        ] {
            let bytes = data(len);
            let manifest = manifest_from_bytes(&bytes, chunk_size);
            assert_eq!(
                manifest.root(),
                blake3::hash(&bytes),
                "merged root differs from blake3::hash at length {len}"
            );
            assert_eq!(manifest.length(), len as u64);
        }
    }

    #[test]
    fn it_holds_for_every_power_of_two_chunk_size() {
        let bytes = data(300_000);
        for shift in 10..=20 {
            let chunk_size = ChunkSize::new(1 << shift).unwrap();
            let manifest = manifest_from_bytes(&bytes, chunk_size);
            assert_eq!(
                manifest.root(),
                blake3::hash(&bytes),
                "merged root differs at chunk size {}",
                1 << shift
            );
        }
    }

    #[test]
    fn a_chunk_verifies_against_its_own_bytes() {
        let chunk_size = ChunkSize::new(1024).unwrap();
        let bytes = data(5000);
        let manifest = manifest_from_bytes(&bytes, chunk_size);
        assert_eq!(manifest.chunk_count(), 5);
        for index in 0..manifest.chunk_count() {
            let (start, len) = manifest.chunk_range(index).unwrap();
            let begin = usize::try_from(start).expect("offset fits in memory");
            let slice = &bytes[begin..begin + len as usize];
            assert!(manifest.verify_chunk(index, slice), "chunk {index} failed");
        }
        assert_eq!(manifest.chunk_range(5), None);
    }

    #[test]
    fn a_flipped_bit_fails_verification() {
        let chunk_size = ChunkSize::new(1024).unwrap();
        let bytes = data(4096);
        let manifest = manifest_from_bytes(&bytes, chunk_size);
        let mut tampered = bytes[1024..2048].to_vec();
        tampered[7] ^= 0x01;
        assert!(!manifest.verify_chunk(1, &tampered));
    }

    #[test]
    fn a_chunk_placed_at_the_wrong_index_fails() {
        // Chaining values depend on where the chunk sits in the file, so a
        // peer cannot pass off chunk 0 as chunk 2.
        let chunk_size = ChunkSize::new(1024).unwrap();
        let bytes = data(4096);
        let manifest = manifest_from_bytes(&bytes, chunk_size);
        assert!(!manifest.verify_chunk(2, &bytes[0..1024]));
    }

    #[test]
    fn a_short_chunk_fails_verification() {
        let chunk_size = ChunkSize::new(1024).unwrap();
        let bytes = data(4096);
        let manifest = manifest_from_bytes(&bytes, chunk_size);
        assert!(!manifest.verify_chunk(0, &bytes[0..1023]));
    }

    #[test]
    fn the_builder_matches_the_whole_file_helper() {
        let chunk_size = ChunkSize::one_mebibyte();
        let bytes = data(2_500_000);
        let mut builder = ManifestBuilder::new(chunk_size);
        for piece in bytes.chunks(chunk_size.as_usize()) {
            builder.push(piece);
        }
        assert_eq!(builder.finish(), manifest_from_bytes(&bytes, chunk_size));
    }
}
