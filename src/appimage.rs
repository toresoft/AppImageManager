//! AppImage format recognition.
//!
//! AppImage type 2 is an ELF executable with the magic `AI\\x02` at offset 8.
//! Its payload is a squashfs filesystem; the squashfs magic `hsqs` (little-endian)
//! sits at the start of the payload. We scan the file (bounded) to find that
//! offset so we can extract single files with `unsquashfs -o <offset> -cat`.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

/// Maximum bytes from the start of the file to scan for the squashfs magic.
/// The ELF header + AppImage header are tiny; the payload starts well before 2 MiB
/// in every real-world AppImage. Bounded scan keeps things fast and safe.
const SCAN_LIMIT: u64 = 2 * 1024 * 1024;

/// Chunk size for the buffered scan.
const CHUNK_SIZE: usize = 64 * 1024;

/// AppImage ELF magic at offset 8..11: `A`, `I`, type byte.
const AI_MAGIC: [u8; 2] = *b"AI";

/// Squashfs magic (little-endian): `hsqs`.
const HSQS: [u8; 4] = *b"hsqs";

/// Error type for AppImage handling.
#[derive(Debug)]
pub enum AppImageError {
    NotFound,
    NotAnAppImage,
    /// Recognised as AppImage but the squashfs payload could not be located.
    NoSquashfs,
    Io(io::Error),
}

impl std::fmt::Display for AppImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppImageError::NotFound => write!(f, "file not found"),
            AppImageError::NotAnAppImage => {
                write!(f, "not an AppImage (missing type-2 AppImage magic)")
            }
            AppImageError::NoSquashfs => {
                write!(
                    f,
                    "AppImage payload (squashfs) not found within the scan window"
                )
            }
            AppImageError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for AppImageError {}

impl From<io::Error> for AppImageError {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::NotFound {
            AppImageError::NotFound
        } else {
            AppImageError::Io(e)
        }
    }
}

/// Inspected AppImage: the byte offset where the squashfs payload begins.
#[derive(Debug, Clone)]
pub struct AppImage {
    pub squashfs_offset: u64,
}

impl AppImage {
    /// Open and validate an AppImage, locating its squashfs payload offset.
    pub fn open(path: &Path) -> Result<Self, AppImageError> {
        let mut file = BufReader::new(File::open(path)?);
        validate_appimage_header(&mut file)?;
        let offset = find_squashfs_offset(&mut file)?;
        Ok(Self {
            squashfs_offset: offset,
        })
    }
}

/// Verify the AppImage type-2 ELF magic (`ELF...AI\\x02`).
fn validate_appimage_header(file: &mut BufReader<File>) -> Result<(), AppImageError> {
    let mut header = [0u8; 11];
    file.read_exact(&mut header)?;
    // ELF magic at 0..4.
    if &header[0..4] != b"\x7fELF" {
        return Err(AppImageError::NotAnAppImage);
    }
    // AppImage type-2 magic at 8..11: `AI` + type byte (0x02).
    if header[8..10] != AI_MAGIC || header[10] != 0x02 {
        return Err(AppImageError::NotAnAppImage);
    }
    Ok(())
}

/// Locate the squashfs payload by scanning for the `hsqs` magic from the file start.
///
/// The naive scan is unreliable because the literal bytes `hsqs` (or any 4-byte
/// pattern) can appear inside the ELF section data. We therefore collect every
/// candidate offset within [`SCAN_LIMIT`] and validate each one against the
/// squashfs superblock: the superblock stores its own `bytes_used` (the size of
/// the filesystem); a valid candidate is one whose `bytes_used` fits within the
/// file. The real payload is the validated candidate; in practice there's
/// exactly one.
///
/// We keep the previous chunk's tail so the magic is detected even if it spans
/// a read boundary.
fn find_squashfs_offset(file: &mut BufReader<File>) -> Result<u64, AppImageError> {
    file.seek(SeekFrom::Start(0))?;

    let file_len = file.get_ref().metadata().map(|m| m.len()).unwrap_or(0);

    let mut candidates = collect_hsqs_candidates(file)?;

    // Sort ascending so the earliest valid payload wins (the AppImage payload
    // is appended after the ELF, so it's near the start of the appended data,
    // but validation is what selects it).
    candidates.sort_unstable();
    candidates.dedup();

    for off in candidates {
        if validate_squashfs_superblock(file, off, file_len) {
            return Ok(off);
        }
    }

    Err(AppImageError::NoSquashfs)
}

/// Collect every offset where the `hsqs` magic appears within [`SCAN_LIMIT`].
fn collect_hsqs_candidates(file: &mut BufReader<File>) -> Result<Vec<u64>, AppImageError> {
    file.seek(SeekFrom::Start(0))?;

    let mut global_pos: u64 = 0;
    let mut buf = vec![0u8; CHUNK_SIZE];
    let mut tail: Vec<u8> = Vec::new();
    let mut candidates = Vec::new();

    loop {
        if global_pos >= SCAN_LIMIT {
            break;
        }
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];

        // All matches inside the current chunk.
        let mut from = 0;
        while let Some(rel) = find_subslice_from(chunk, &HSQS, from) {
            candidates.push(global_pos + rel as u64);
            from = rel + HSQS.len();
        }

        // Match across the previous tail / current head boundary.
        if !tail.is_empty() {
            let head_len = HSQS.len() - 1;
            let head = &chunk[..head_len.min(chunk.len())];
            let mut overlap = Vec::with_capacity(tail.len() + head.len());
            overlap.extend_from_slice(&tail);
            overlap.extend_from_slice(head);
            let mut from = 0;
            while let Some(rel) = find_subslice_from(&overlap, &HSQS, from) {
                let abs = global_pos - tail.len() as u64 + rel as u64;
                if abs < SCAN_LIMIT {
                    candidates.push(abs);
                }
                from = rel + HSQS.len();
            }
        }

        // Keep the last `HSQS.len() - 1` bytes as the next iteration's tail.
        let keep = HSQS.len() - 1;
        if chunk.len() >= keep {
            tail.clear();
            tail.extend_from_slice(&chunk[chunk.len() - keep..]);
        } else {
            tail.clear();
            tail.extend_from_slice(chunk);
        }

        global_pos += n as u64;
    }

    Ok(candidates)
}

/// Validate a candidate squashfs superblock at `offset`.
///
/// Squashfs superblock layout (little-endian), per `squashfs_fs.h`:
/// ```text
///  0  u32 magic            (`hsqs`)
///  4  u32 inodes
///  8  u32 mkfs_time
/// 12  u32 block_size
/// 16  u32 fragments
/// 20  u16 compression
/// 22  u16 block_log
/// 24  u16 flags
/// 26  u16 id_count
/// 28  u16 version_major
/// 30  u16 version_minor
/// 32  u64 root_inode
/// 40  u64 bytes_used
/// ```
/// We sanity-check that `block_size == 1 << block_log`, that `block_log` is in
/// a plausible range, and that `bytes_used` fits within the file. These cheap
/// checks reject spurious `hsqs` matches inside ELF data reliably.
fn validate_squashfs_superblock(file: &mut BufReader<File>, offset: u64, file_len: u64) -> bool {
    if file_len > 0 && offset + 48 > file_len {
        return false;
    }
    if file
        .seek(SeekFrom::Start(offset))
        .map(|s| s != offset)
        .unwrap_or(true)
    {
        return false;
    }
    let mut hdr = [0u8; 48];
    if file.read_exact(&mut hdr).is_err() {
        return false;
    }
    if hdr[0..4] != HSQS {
        return false;
    }
    let block_size = u32::from_le_bytes([hdr[12], hdr[13], hdr[14], hdr[15]]);
    let block_log = u16::from_le_bytes([hdr[22], hdr[23]]) as u32;
    let bytes_used = u64::from_le_bytes([
        hdr[40], hdr[41], hdr[42], hdr[43], hdr[44], hdr[45], hdr[46], hdr[47],
    ]);

    // block_size must equal 1 << block_log, in [4KiB, 1MiB].
    if !(12..=20).contains(&block_log) {
        return false;
    }
    if block_size != (1u32 << block_log) {
        return false;
    }
    if bytes_used < 48 {
        return false;
    }
    if file_len > 0 && offset + bytes_used > file_len {
        // Trailing AppImage signatures live *after* the squashfs payload, so
        // the payload must end at or before EOF.
        return false;
    }
    true
}

/// Like [`find_subslice`] but starts searching from `from`.
fn find_subslice_from(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() || from >= haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}
