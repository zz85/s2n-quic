// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Direct (unbuffered) file IO primitive — a port of an existing direct-IO crate.
//!
//! Provides page-aligned buffer allocation and an [`O_DIRECT`](open_direct_file)/`F_NOCACHE` file
//! handle with positional `read_at`/`write_at`/`sync`. The blocking-syscall backend
//! ([`crate::fs::backend::syscall`]) runs these on its bounded worker pool.
//!
//! `O_DIRECT` requires the buffer pointer, the file offset, and the length to all be aligned to the
//! device block size (here [`ALIGNMENT`] = 4 KiB). [`AlignedBuf`] guarantees the pointer and a
//! page-rounded capacity; callers are responsible for aligned offsets/lengths. On tmpfs (where
//! `O_DIRECT` is rejected) and on macOS the open path falls back appropriately.

use s2n_quic_core::buffer::writer::Storage as _;
use std::{
    fs::OpenOptions,
    io,
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
};

/// Device block alignment for direct IO. 4 KiB covers every common page/sector size.
pub const ALIGNMENT: usize = 4096;

/// Round `n` up to the next multiple of [`ALIGNMENT`], **saturating** at the largest aligned value
/// rather than overflowing. `n + ALIGNMENT - 1` would wrap for `n > usize::MAX - ALIGNMENT + 1`;
/// since `align_up` sizes an `AlignedBuf` allocation whose `as_slice`/`as_mut_slice` build slices via
/// unsafe, a wrapped (undersized) `cap` would be a soundness hole — so we saturate (an allocation of
/// that size fails cleanly in `Layout`/`alloc` instead).
#[inline]
pub fn align_up(n: usize) -> usize {
    match n.checked_add(ALIGNMENT - 1) {
        Some(s) => s & !(ALIGNMENT - 1),
        None => usize::MAX & !(ALIGNMENT - 1),
    }
}

/// A page-aligned heap buffer for direct IO. The allocation pointer is aligned to [`ALIGNMENT`] and
/// the capacity is rounded up to a whole number of pages, so a read/write of an aligned length is a
/// valid `O_DIRECT` transfer.
///
/// This is the caller-owned buffer for **zero-copy** direct IO: the application fills it (write) or
/// receives into it (read) with no intermediate copy in the data path. Its pointer is always
/// [`ALIGNMENT`]-aligned and its `aligned_len` is a whole number of pages, satisfying two of
/// `O_DIRECT`'s three constraints; the third (a block-aligned file offset) is the caller's
/// responsibility and is validated at submit time.
pub struct AlignedBuf {
    ptr: std::ptr::NonNull<u8>,
    /// Logical length exposed to callers (may be < `cap`).
    len: usize,
    /// Page-rounded allocation size.
    cap: usize,
}

impl core::fmt::Debug for AlignedBuf {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AlignedBuf")
            .field("len", &self.len)
            .field("cap", &self.cap)
            .finish()
    }
}

// SAFETY: `AlignedBuf` owns its allocation exclusively; the bytes are plain data.
unsafe impl Send for AlignedBuf {}
unsafe impl Sync for AlignedBuf {}

impl AlignedBuf {
    /// Allocate a zeroed buffer whose capacity is `align_up(len)` and whose logical length is `len`.
    pub fn new(len: usize) -> Self {
        let cap = align_up(len.max(1));
        let layout =
            std::alloc::Layout::from_size_align(cap, ALIGNMENT).expect("valid aligned layout");
        // SAFETY: layout has non-zero size (cap >= ALIGNMENT) and valid alignment.
        let raw = unsafe { std::alloc::alloc_zeroed(layout) };
        let ptr =
            std::ptr::NonNull::new(raw).unwrap_or_else(|| std::alloc::handle_alloc_error(layout));
        Self { ptr, len, cap }
    }

    /// Allocate a buffer holding `reader`'s bytes, **block-aligned and ready for a direct write**.
    ///
    /// The reader's bytes are copied into the front of a page-rounded allocation, then the logical
    /// `len` is **padded up to the next [`ALIGNMENT`] boundary**, with the padding bytes zeroed. The
    /// result is therefore safe to hand straight to an `O_DIRECT` write — whose transfer length must
    /// be block-aligned — without the caller having to round `len` up themselves. Since this buffer is
    /// almost always destined for a write, padding by default avoids the easy footgun of submitting an
    /// unaligned length and getting an `InvalidInput` rejection at submit time.
    ///
    /// A caller that wants only the payload (e.g. to inspect it, or to size a non-direct write) can
    /// [`set_len`](Self::set_len) back down to the unpadded length — the data prefix is unchanged.
    ///
    /// Unlike [`new`](Self::new) — which `alloc_zeroed`s the whole capacity — this only memsets the
    /// padding tail, not the data region the copy is about to overwrite. The whole `cap` stays
    /// initialized (data prefix from the copy, remainder zeroed), so `as_slice` never exposes
    /// uninitialized memory at any valid `len`. The reader is
    /// [`Infallible`](s2n_quic_core::buffer::reader::storage::Infallible), so the copy cannot fail,
    /// and the destination capacity always covers `buffered_len`, so the reader is fully drained.
    pub fn copy_from_reader<R>(reader: &mut R) -> Self
    where
        R: s2n_quic_core::buffer::reader::storage::Infallible + ?Sized,
    {
        let len = reader.buffered_len();
        let cap = align_up(len.max(1));
        let layout =
            std::alloc::Layout::from_size_align(cap, ALIGNMENT).expect("valid aligned layout");
        // SAFETY: layout has non-zero size (cap >= ALIGNMENT) and valid alignment.
        let raw = unsafe { std::alloc::alloc(layout) };
        let ptr =
            std::ptr::NonNull::new(raw).unwrap_or_else(|| std::alloc::handle_alloc_error(layout));

        // Copy the reader into the `len`-byte prefix via the writer-storage machinery, tracking how
        // many bytes actually land (== `buffered_len` for a well-behaved reader, but we trust the
        // count, not the request).
        // SAFETY: `ptr` is valid for writes of `len` bytes (`len <= cap`).
        let mut dest = unsafe { bytes::buf::UninitSlice::from_raw_parts_mut(ptr.as_ptr(), len) };
        let mut tracked = dest.track_write();
        reader.infallible_copy_into(&mut tracked);
        let written = tracked.written_len();

        // Zero the unused tail `[written, cap)` so the entire allocation is initialized: this is the
        // only memset, and it covers the alignment padding written to disk. (Disjoint from the copied
        // prefix; for `written == cap` this writes nothing.)
        // SAFETY: `ptr + written` is in-bounds and valid for `cap - written` bytes (`written <= cap`).
        unsafe {
            core::ptr::write_bytes(ptr.as_ptr().add(written), 0, cap - written);
        }

        // Pad the logical length up to a block boundary so the buffer is directly writable. This is
        // `<= cap` (`written <= buffered_len`, and `cap == align_up(buffered_len)`), and the
        // `[written, len)` padding it exposes was just zeroed. An empty reader stays `len == 0`.
        Self {
            ptr,
            len: align_up(written),
            cap,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The page-rounded allocation capacity (always a whole number of [`ALIGNMENT`] pages, and
    /// `>= len`). This is the maximum the logical length can be set to, not necessarily what a given
    /// transfer moves.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.cap
    }

    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: `ptr` is valid for `len` initialized bytes. `new` zero-initializes all `cap`;
        // `copy_from_reader` initializes the `len`-byte prefix it copies into. `len <= cap` and
        // bytes `[0, len)` stay initialized — `set_len` only grows within already-initialized
        // bounds (it asserts `len <= cap`; callers never grow past initialized data).
        unsafe { core::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    /// The logical-length mutable slice, for an in-place direct read destination (zero-copy).
    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: `ptr` is valid for `len` initialized bytes (`len <= cap`, and bytes `[0, len)` are
        // initialized by `new`/`copy_from_reader`) and we hold `&mut self`.
        unsafe { core::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }

    /// Set the logical length, which must be `<= capacity`. The bound is a hard `assert!` (not a
    /// `debug_assert!`) because `as_slice`/`as_mut_slice` build slices of `len` bytes via unsafe —
    /// a `len > cap` would make those slices read/write out of bounds (UB), so the check must hold
    /// in release builds too.
    #[inline]
    pub fn set_len(&mut self, len: usize) {
        assert!(
            len <= self.cap,
            "AlignedBuf::set_len({len}) exceeds capacity {}",
            self.cap
        );
        self.len = len;
    }

    /// True if the pointer is [`ALIGNMENT`]-aligned (always true for `AlignedBuf`).
    #[inline]
    pub fn is_aligned(&self) -> bool {
        (self.ptr.as_ptr() as usize).is_multiple_of(ALIGNMENT)
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        let layout =
            std::alloc::Layout::from_size_align(self.cap, ALIGNMENT).expect("valid aligned layout");
        // SAFETY: `ptr`/`cap`/alignment match the original allocation.
        unsafe { std::alloc::dealloc(self.ptr.as_ptr(), layout) };
    }
}

impl AsRef<[u8]> for AlignedBuf {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

/// Options for opening a direct-IO file.
pub struct Options {
    /// Truncate the file on open.
    pub truncate: bool,
    /// Pre-size the file to this many bytes (use 0 to leave the size unchanged).
    pub size: u64,
    /// Open with `O_DIRECT`/`F_NOCACHE`. Set false for tmpfs/test files where direct IO is rejected.
    pub direct: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            truncate: false,
            size: 0,
            direct: true,
        }
    }
}

/// A direct-IO file handle with positional read/write. Cheap to clone the path; the underlying
/// `std::fs::File` is shared by the worker pool via the fd it exposes.
pub struct File {
    inner: std::fs::File,
    path: PathBuf,
    /// Opened with [`Options::direct`], so [`read_at`](Self::read_at) and
    /// [`write_at`](Self::write_at) hold every call to direct IO's alignment.
    direct: bool,
}

impl std::os::fd::AsRawFd for File {
    /// Lets `Arc<File>` coerce to `Arc<dyn AsRawFd + Send + Sync>` so it can back an
    /// [`Fd`](crate::fs::op::Fd) — the file then stays open for an op's whole lifetime.
    #[inline]
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.inner.as_raw_fd()
    }
}

impl File {
    /// Open `path` per `options`, optionally in direct (unbuffered) mode.
    pub fn open(path: impl AsRef<Path>, options: Options) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let inner = open_file(&path, options.truncate, options.direct)?;
        if options.size > 0 {
            inner.set_len(options.size)?;
        }
        Ok(Self {
            inner,
            path,
            direct: options.direct,
        })
    }

    /// The raw fd, for handing to a backend (e.g. an `IoOp`'s `fd` field).
    #[inline]
    pub fn raw_fd(&self) -> i32 {
        use std::os::fd::AsRawFd;
        self.inner.as_raw_fd()
    }

    #[inline]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read exactly `buf.len()` bytes at `offset`.
    ///
    /// On a file opened with [`Options::direct`], `buf`'s address and length and `offset` must be
    /// multiples of [`ALIGNMENT`], as an [`AlignedBuf`] is; see [`write_at`](Self::write_at).
    pub fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        self.check_direct_alignment(buf.as_ptr(), buf.len(), offset)?;
        self.inner.read_exact_at(buf, offset)
    }

    /// Write all of `buf` at `offset`.
    ///
    /// On a file opened with [`Options::direct`], `buf`'s address and length and `offset` must be
    /// multiples of [`ALIGNMENT`], as an [`AlignedBuf`] is. Otherwise the call fails with
    /// `InvalidInput` before it reaches the kernel. The kernel's own check depends on the device:
    /// a disk whose DMA alignment is 512 bytes refuses a buffer that a disk with 4-byte DMA
    /// alignment takes, and tmpfs, where the file isn't opened `O_DIRECT`, takes any buffer. This
    /// check makes a misaligned buffer fail the same way on every device.
    pub fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<()> {
        self.check_direct_alignment(buf.as_ptr(), buf.len(), offset)?;
        self.inner.write_all_at(buf, offset)
    }

    /// Refuses, on a file opened with [`Options::direct`], a buffer whose address or length, or an
    /// offset, isn't a multiple of [`ALIGNMENT`].
    fn check_direct_alignment(&self, addr: *const u8, len: usize, offset: u64) -> io::Result<()> {
        if !self.direct
            || ((addr as usize).is_multiple_of(ALIGNMENT)
                && len.is_multiple_of(ALIGNMENT)
                && offset.is_multiple_of(ALIGNMENT as u64))
        {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "direct file: buffer address, buffer length or offset is not block-aligned",
        ))
    }

    /// Flush file data + metadata to the device.
    pub fn sync(&self) -> io::Result<()> {
        self.inner.sync_all()
    }

    /// Flush file data (not necessarily metadata) to the device.
    pub fn sync_data(&self) -> io::Result<()> {
        self.inner.sync_data()
    }
}

/// Open a file, optionally in direct (unbuffered) mode. On Linux uses `O_DIRECT` (skipped on tmpfs,
/// which rejects it); on macOS sets `F_NOCACHE` after open.
fn open_file(path: &Path, truncate: bool, direct: bool) -> io::Result<std::fs::File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .truncate(truncate);

    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        if direct && !is_tmpfs(path).unwrap_or(true) {
            options.custom_flags(libc::O_DIRECT);
        }
        options.open(path)
    }

    #[cfg(target_os = "macos")]
    {
        let file = options.open(path)?;
        if direct {
            use std::os::fd::AsRawFd;
            // SAFETY: `file` owns a valid fd for the duration of this call.
            let ret = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_NOCACHE, 1) };
            if ret < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(file)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = direct;
        options.open(path)
    }
}

/// True if `path`'s parent directory is on tmpfs (where `O_DIRECT` is unsupported).
#[cfg(target_os = "linux")]
fn is_tmpfs(path: &Path) -> io::Result<bool> {
    use std::{ffi::CString, mem::MaybeUninit};

    let dir = path.parent().unwrap_or(path);
    let cstr = CString::new(dir.as_os_str().as_encoded_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains null byte"))?;
    let mut statfs = MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `cstr` is a valid C string; `statfs` is a valid out pointer.
    let ret = unsafe { libc::statfs(cstr.as_ptr(), statfs.as_mut_ptr()) };
    if ret != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `statfs` was initialized by a successful `statfs` call.
    let statfs = unsafe { statfs.assume_init() };
    Ok(statfs.f_type == libc::TMPFS_MAGIC)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligned_buf_is_aligned_and_page_rounded() {
        let buf = AlignedBuf::new(100);
        assert!(buf.is_aligned());
        assert_eq!(buf.len(), 100);
        assert_eq!(buf.capacity(), ALIGNMENT);
        let buf = AlignedBuf::new(ALIGNMENT + 1);
        assert_eq!(buf.capacity(), 2 * ALIGNMENT);
    }

    #[test]
    fn copy_from_reader_pads_to_block() {
        // A sub-page source: the logical length is padded up to a block boundary by default, with
        // the data copied into the front and the padding zeroed — ready for a direct write.
        let src = b"hello direct io world";
        let mut reader: &[u8] = src;
        let buf = AlignedBuf::copy_from_reader(&mut reader);

        assert!(buf.is_aligned());
        assert_eq!(buf.len(), ALIGNMENT, "len padded to a block boundary");
        assert_eq!(buf.capacity(), ALIGNMENT);
        // The reader was fully drained.
        assert!(reader.is_empty());
        // Data prefix is intact; the rest of the block is zero padding.
        assert_eq!(&buf.as_slice()[..src.len()], src);
        assert!(buf.as_slice()[src.len()..].iter().all(|&b| b == 0));

        // A caller wanting only the payload can shrink back; the prefix is unchanged.
        let mut buf = buf;
        buf.set_len(src.len());
        assert_eq!(buf.as_slice(), src);
    }

    #[test]
    fn copy_from_reader_multi_page_and_empty() {
        // A source spanning more than one page rounds capacity (and the padded len) up to the next
        // page boundary.
        let src = vec![0xABu8; ALIGNMENT + 1];
        let mut reader: &[u8] = &src;
        let buf = AlignedBuf::copy_from_reader(&mut reader);
        assert_eq!(buf.len(), 2 * ALIGNMENT, "len padded to the next block");
        assert_eq!(buf.capacity(), 2 * ALIGNMENT);
        assert_eq!(&buf.as_slice()[..src.len()], &src[..]);
        assert!(buf.as_slice()[src.len()..].iter().all(|&b| b == 0));

        // An exactly block-aligned source needs no padding: len stays as-is.
        let src = vec![0xCDu8; ALIGNMENT];
        let mut reader: &[u8] = &src;
        let buf = AlignedBuf::copy_from_reader(&mut reader);
        assert_eq!(buf.len(), ALIGNMENT);
        assert_eq!(buf.as_slice(), &src[..]);

        // An empty reader still allocates a valid (one-page, zeroed) buffer with logical length 0 —
        // there is nothing to pad.
        let mut empty: &[u8] = &[];
        let buf = AlignedBuf::copy_from_reader(&mut empty);
        assert_eq!(buf.len(), 0);
        assert_eq!(buf.capacity(), ALIGNMENT);
        assert!(buf.is_aligned());
    }

    #[test]
    fn align_up_rounds() {
        assert_eq!(align_up(0), 0);
        assert_eq!(align_up(1), ALIGNMENT);
        assert_eq!(align_up(ALIGNMENT), ALIGNMENT);
        assert_eq!(align_up(ALIGNMENT + 1), 2 * ALIGNMENT);
    }

    #[test]
    fn buffered_read_write_roundtrip() {
        // Use a temp file in buffered mode (no O_DIRECT) so the test is portable across filesystems.
        let dir = std::env::temp_dir();
        let path = dir.join(format!("s2n-dc-direct-test-{}", std::process::id()));
        let file = File::open(
            &path,
            Options {
                truncate: true,
                size: 0,
                direct: false,
            },
        )
        .unwrap();

        let data = b"hello direct io world";
        file.write_at(data, 0).unwrap();
        file.sync().unwrap();

        let mut buf = vec![0u8; data.len()];
        file.read_at(&mut buf, 0).unwrap();
        assert_eq!(&buf, data);

        let _ = std::fs::remove_file(&path);
    }

    /// A file opened `direct` refuses, with `InvalidInput`, a read or write whose buffer address,
    /// buffer length or offset isn't block-aligned, whatever the device underneath would take, and
    /// takes an aligned one. Here the temp dir may be tmpfs, where the file isn't opened
    /// `O_DIRECT`, or a disk that takes 4-byte-aligned buffers: there the kernel takes each
    /// misaligned call below, and only this check refuses it.
    #[test]
    fn direct_file_refuses_misaligned_io() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("s2n-dc-direct-align-test-{}", std::process::id()));
        let file = File::open(
            &path,
            Options {
                truncate: true,
                size: 2 * ALIGNMENT as u64,
                direct: true,
            },
        )
        .unwrap();
        let mut buf = AlignedBuf::new(2 * ALIGNMENT);
        let misaligned = |result: io::Result<()>, what: &str| {
            let err = result.expect_err(what);
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{what}: {err}");
        };

        // A buffer 16 bytes past a page boundary: the address a `Vec<u8>` from the allocator has.
        misaligned(
            file.write_at(&buf.as_slice()[16..16 + ALIGNMENT], 0),
            "a write from a misaligned address",
        );
        misaligned(
            file.read_at(&mut buf.as_mut_slice()[16..16 + ALIGNMENT], 0),
            "a read into a misaligned address",
        );
        misaligned(
            file.write_at(&buf.as_slice()[..100], 0),
            "a write of a length that isn't a whole block",
        );
        misaligned(
            file.write_at(&buf.as_slice()[..ALIGNMENT], 100),
            "a write at an offset inside a block",
        );

        buf.as_mut_slice()[..ALIGNMENT].fill(7);
        file.write_at(&buf.as_slice()[..ALIGNMENT], ALIGNMENT as u64)
            .expect("an aligned write");
        buf.as_mut_slice()[ALIGNMENT..].fill(0);
        file.read_at(&mut buf.as_mut_slice()[ALIGNMENT..], ALIGNMENT as u64)
            .expect("an aligned read");
        assert!(buf.as_slice()[ALIGNMENT..].iter().all(|&b| b == 7));

        drop(file);
        let _ = std::fs::remove_file(&path);
    }
}
