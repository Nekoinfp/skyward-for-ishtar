//! Android sparse images: a complete format pass before writing, followed by a
//! bounded reader/writer pipeline. DON'T CARE means preserve, never discard.

use anyhow::{bail, ensure, Context, Result};
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::mpsc;
use std::thread;

const MAGIC: [u8; 4] = [0x3a, 0xff, 0x26, 0xed];
const HEADER: u64 = 28;
const CHUNK_HEADER: u64 = 12;
const BUFFER: usize = 4 << 20;
const BUFFERS: usize = 4;
const RAW: u16 = 0xcac1;
const FILL: u16 = 0xcac2;
const DONT_CARE: u16 = 0xcac3;
const CRC32: u16 = 0xcac4;

#[derive(Clone, Copy, Debug)]
enum Chunk {
    Raw { source: u64, len: u64 },
    Fill { pattern: [u8; 4], len: u64 },
    Skip { len: u64 },
    Crc(u32),
}

#[derive(Debug)]
pub struct Sparse {
    len: u64,
    chunks: Vec<Chunk>,
}

struct Work {
    skip: Option<u64>,
    bytes: Vec<u8>,
    crc: Option<(u32, u32)>,
}

/// Parse every header and payload boundary. No device has to be opened first.
pub fn plan<R: Read + Seek>(input: &mut R, file_size: u64) -> Result<Sparse> {
    input.rewind().context("rewind sparse image")?;
    let mut head = [0u8; HEADER as usize];
    input.read_exact(&mut head).context("read sparse header")?;
    ensure!(head[..4] == MAGIC, "not an Android sparse image");
    let short = |at| u16::from_le_bytes(head[at..at + 2].try_into().unwrap());
    let long = |at| u32::from_le_bytes(head[at..at + 4].try_into().unwrap());
    ensure!(short(4) == 1 && short(6) == 0, "unsupported sparse version {}.{}", short(4), short(6));
    let file_header = u64::from(short(8));
    let chunk_header = u64::from(short(10));
    ensure!(file_header >= HEADER && chunk_header >= CHUNK_HEADER, "short sparse header");
    let block = u64::from(long(12));
    ensure!(block != 0 && block.is_multiple_of(4), "sparse block size {block} is not four-byte aligned");
    ensure!(block <= 64 << 20, "sparse block size {block} is too large");
    let len = block.checked_mul(u64::from(long(16))).context("expanded size overflow")?;
    let chunks_count = long(20);
    // A chunk takes at least its own header out of the file, so a count the
    // file could not hold is a damaged image rather than a big one. Checking
    // it before anything is set aside for the chunks keeps a header that names
    // four billion of them from asking for the room first.
    let most = file_size / CHUNK_HEADER;
    ensure!(
        u64::from(chunks_count) <= most,
        "{chunks_count} chunks do not fit in a file of {file_size} bytes"
    );
    let mut at = file_header;
    let mut out_at = 0u64;
    let mut chunks = Vec::with_capacity(chunks_count as usize);
    for number in 0..chunks_count {
        ensure!(file_size.checked_sub(at).is_some_and(|left| left >= CHUNK_HEADER), "truncated sparse chunk header {number}");
        input.seek(SeekFrom::Start(at))?;
        let mut h = [0u8; CHUNK_HEADER as usize];
        input.read_exact(&mut h)?;
        let kind = u16::from_le_bytes(h[..2].try_into().unwrap());
        let blocks = u64::from(u32::from_le_bytes(h[4..8].try_into().unwrap()));
        let total = u64::from(u32::from_le_bytes(h[8..12].try_into().unwrap()));
        let payload = total.checked_sub(chunk_header).context("sparse chunk shorter than its header")?;
        let chunk_len = block.checked_mul(blocks).context("sparse chunk size overflow")?;
        out_at = out_at.checked_add(chunk_len).context("expanded offset overflow")?;
        ensure!(out_at <= len, "sparse chunk {number} expands beyond total_blocks");
        let source = at.checked_add(chunk_header).context("sparse source offset overflow")?;
        ensure!(file_size.checked_sub(source).is_some_and(|left| left >= payload), "truncated sparse chunk {number} payload");
        let chunk = match kind {
            RAW if payload == chunk_len => Chunk::Raw { source, len: chunk_len },
            FILL if payload == 4 => {
                // The payload starts past the chunk header, which may be
                // longer than the twelve bytes every chunk begins with.
                input.seek(SeekFrom::Start(source))?;
                let mut pattern = [0; 4];
                input.read_exact(&mut pattern)?;
                Chunk::Fill { pattern, len: chunk_len }
            }
            DONT_CARE if payload == 0 => Chunk::Skip { len: chunk_len },
            CRC32 if payload == 4 && blocks == 0 => {
                input.seek(SeekFrom::Start(source))?;
                let mut value = [0; 4];
                input.read_exact(&mut value)?;
                Chunk::Crc(u32::from_le_bytes(value))
            }
            RAW | FILL | DONT_CARE | CRC32 => bail!("sparse chunk {number} of type {kind:#x} has an invalid size"),
            _ => bail!("unsupported sparse chunk {kind:#x} at {number}"),
        };
        chunks.push(chunk);
        at = source.checked_add(payload).context("sparse offset overflow")?;
    }
    ensure!(out_at == len, "chunks expand to {out_at}, not the declared {len}");
    // Bytes past the last chunk are not part of the image: some tools pad the
    // file to a round size. Nothing is read from them and they say nothing
    // about what the image holds, so they are no reason to refuse it. A chunk
    // that claims more than the file has was already refused above.
    Ok(Sparse { len, chunks })
}

/// Expanded image size, including preserved ranges.
pub fn expanded_size<R: Read + Seek>(input: &mut R, file_size: u64) -> Result<Option<u64>> {
    let at = input.stream_position()?;
    let mut magic = [0; 4];
    let sparse = input.read_exact(&mut magic).is_ok() && magic == MAGIC;
    input.seek(SeekFrom::Start(at))?;
    if sparse { plan(input, file_size).map(|s| Some(s.len)) } else { Ok(None) }
}

/// Write a sparse image to a device which has already been checked for capacity.
/// CRC32 and write failures are reported; verification of written media remains
/// the caller's responsibility. No discard is performed.
pub fn flash<R: Read + Seek + Send, W: Write + Seek>(input: &mut R, file_size: u64, out: &mut W, progress: &mut dyn FnMut(u64, u64)) -> Result<()> {
    let sparse = plan(input, file_size)?;
    let expanded = sparse.len;
    let (free_tx, free_rx) = mpsc::sync_channel::<Vec<u8>>(BUFFERS);
    let (full_tx, full_rx) = mpsc::sync_channel::<Result<Work>>(BUFFERS);
    for _ in 0..BUFFERS {
        free_tx.send(Vec::with_capacity(BUFFER)).expect("new buffer queue");
    }
    thread::scope(|scope| -> Result<()> {
        let reader = scope.spawn(move || {
            let mut crc = crc32fast::Hasher::new();
            let mut position = 0u64;
            let mut queued = 0u64;
            'chunks: for chunk in sparse.chunks {
                match chunk {
                    Chunk::Skip { len } => {
                        // Android's sparse CRC covers the expanded image; holes
                        // are zero for that calculation, but not on the device.
                        write_zeroes(&mut crc, len);
                        queued += len;
                        let Ok(bytes) = free_rx.recv() else { return };
                        if full_tx.send(Ok(Work { skip: Some(queued), bytes, crc: None })).is_err() { return; }
                        position = queued;
                    }
                    Chunk::Crc(want) => {
                        let got = crc.clone().finalize();
                        let Ok(bytes) = free_rx.recv() else { return };
                        if full_tx.send(Ok(Work { skip: None, bytes, crc: Some((want, got)) })).is_err() { return; }
                        if got != want { return; }
                        crc = crc32fast::Hasher::new();
                    }
                    Chunk::Raw { source, mut len } => {
                        let mut consumed = 0u64;
                        while len != 0 {
                            let Ok(mut bytes) = free_rx.recv() else { return };
                            let n = len.min(BUFFER as u64) as usize;
                            bytes.resize(n, 0);
                            let read = input.seek(SeekFrom::Start(source + consumed)).and_then(|_| input.read_exact(&mut bytes))
                                .with_context(|| format!("read sparse raw chunk at {}", source + consumed));
                            if let Err(e) = read {
                                let _ = full_tx.send(Err(e));
                                return;
                            }
                            crc.update(&bytes);
                            consumed += n as u64;
                            len -= n as u64;
                            queued += n as u64;
                            position = queued;
                            if full_tx.send(Ok(Work { skip: None, bytes, crc: None })).is_err() { return; }
                        }
                    }
                    Chunk::Fill { pattern, mut len } => {
                        let mut consumed = 0u64;
                        while len != 0 {
                            let Ok(mut bytes) = free_rx.recv() else { return };
                            let n = len.min(BUFFER as u64) as usize;
                            bytes.resize(n, 0);
                            for (i, b) in bytes.iter_mut().enumerate() { *b = pattern[(consumed as usize + i) % 4]; }
                            crc.update(&bytes);
                            consumed += n as u64;
                            len -= n as u64;
                            queued += n as u64;
                            position = queued;
                            if full_tx.send(Ok(Work { skip: None, bytes, crc: None })).is_err() { return; }
                        }
                    }
                }
                if position > expanded { break 'chunks; }
            }
        });
        let mut done = 0;
        let result = (|| -> Result<()> {
            while let Ok(item) = full_rx.recv() {
                let mut item = item?;
                if let Some(to) = item.skip { out.seek(SeekFrom::Start(to)).context("seek past sparse don't-care range")?; }
                if !item.bytes.is_empty() {
                    out.write_all(&item.bytes).context("write sparse output")?;
                    done += item.bytes.len() as u64;
                    progress(done, expanded);
                }
                if let Some((want, got)) = item.crc {
                    ensure!(want == got, "sparse CRC32 mismatch: expanded image has {got:08x}, not {want:08x}");
                }
                item.bytes.clear();
                // A reader that has stopped — a CRC failure, say — is gone,
                // and asking it for a buffer cannot work. What it has already
                // queued still has to be taken in and judged, so this is not a
                // reason to stop: the loop ends when nothing more arrives.
                let _ = free_tx.send(item.bytes);
            }
            Ok(())
        })();
        drop(free_tx);
        drop(full_rx);
        reader.join().map_err(|_| anyhow::anyhow!("sparse reader panicked"))?;
        result
    })
}

/// Whether the bytes a sparse image expands to at `at` are `magic`.
///
/// A sparse super image keeps super's geometry past its first page, and one
/// chunk covers that offset: read those four bytes without expanding the
/// whole image. A hole reads as whatever the device already holds, which
/// cannot be told from here, so it is not taken for super's geometry.
pub fn expands_to<R: Read + Seek>(input: &mut R, at: u64, magic: [u8; 4]) -> Result<bool> {
    let file_size = input.seek(SeekFrom::End(0))?;
    input.rewind()?;
    let mut head = [0u8; HEADER as usize];
    input.read_exact(&mut head).context("read sparse header")?;
    ensure!(head[..4] == MAGIC, "not an Android sparse image");
    let block = u64::from(u32::from_le_bytes(head[12..16].try_into().expect("four bytes")));
    ensure!(block != 0 && block.is_multiple_of(4), "invalid sparse block size {block}");
    let file_header = u64::from(u16::from_le_bytes(head[8..10].try_into().expect("two bytes")));
    let chunk_header = u64::from(u16::from_le_bytes(head[10..12].try_into().expect("two bytes")));
    ensure!(file_header >= HEADER && chunk_header >= CHUNK_HEADER, "short sparse header");
    let mut file_at = file_header;
    let mut out_at = 0u64;
    for _ in 0..u32::from_le_bytes(head[20..24].try_into().expect("four bytes")) {
        ensure!(file_size.checked_sub(file_at).is_some_and(|left| left >= CHUNK_HEADER), "truncated sparse chunk header");
        input.seek(SeekFrom::Start(file_at))?;
        let mut h = [0u8; CHUNK_HEADER as usize];
        input.read_exact(&mut h)?;
        let kind = u16::from_le_bytes(h[..2].try_into().expect("two bytes"));
        let blocks = u64::from(u32::from_le_bytes(h[4..8].try_into().expect("four bytes")));
        let total = u64::from(u32::from_le_bytes(h[8..12].try_into().expect("four bytes")));
        let payload = total.checked_sub(chunk_header).context("sparse chunk shorter than its header")?;
        let len = block.checked_mul(blocks).context("sparse chunk size overflow")?;
        let source = file_at.checked_add(chunk_header).context("sparse chunk offset overflow")?;
        ensure!(file_size.checked_sub(source).is_some_and(|left| left >= payload), "truncated sparse chunk payload");
        let end = out_at.checked_add(len).context("expanded offset overflow")?;
        if out_at <= at && at + 4 <= end {
            let offset = at - out_at;
            let mut bytes = [0u8; 4];
            match kind {
                RAW => {
                    input.seek(SeekFrom::Start(source + offset))?;
                    input.read_exact(&mut bytes)?;
                }
                FILL => {
                    let mut pattern = [0u8; 4];
                    input.seek(SeekFrom::Start(source))?;
                    input.read_exact(&mut pattern)?;
                    for (i, byte) in bytes.iter_mut().enumerate() {
                        *byte = pattern[(offset as usize + i) % 4];
                    }
                }
                _ => return Ok(false),
            }
            return Ok(bytes == magic);
        }
        out_at = end;
        file_at = source.checked_add(payload).context("sparse offset overflow")?;
    }
    Ok(false)
}

fn write_zeroes(crc: &mut crc32fast::Hasher, mut len: u64) {
    let zeros = [0u8; 64 * 1024];
    while len != 0 {
        let n = len.min(zeros.len() as u64) as usize;
        crc.update(&zeros[..n]);
        len -= n as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn image(block: u32, chunks: &[(u16, u32, &[u8])], crc: Option<u32>) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(MAGIC);
        for n in [1u16, 0, 28, 12] { out.extend(n.to_le_bytes()); }
        for n in [block, chunks.iter().map(|c| c.1).sum(), chunks.len() as u32 + u32::from(crc.is_some()), 0] { out.extend(n.to_le_bytes()); }
        for (kind, blocks, data) in chunks {
            out.extend(kind.to_le_bytes()); out.extend(0u16.to_le_bytes());
            out.extend(blocks.to_le_bytes()); out.extend((12 + data.len() as u32).to_le_bytes()); out.extend(*data);
        }
        if let Some(crc) = crc {
            out.extend(CRC32.to_le_bytes()); out.extend(0u16.to_le_bytes()); out.extend(0u32.to_le_bytes()); out.extend(16u32.to_le_bytes()); out.extend(crc.to_le_bytes());
        }
        out
    }

    #[test]
    fn chunks_longer_than_one_buffer_are_written_whole() {
        // Every chunk here is longer than the four-megabyte buffer the reader
        // hands over, so each is split across several of them; the hole ends
        // on none of the buffer's edges either.
        const BLOCK: u32 = 4096;
        let blocks = |bytes: usize| (bytes / BLOCK as usize) as u32;
        let raw: Vec<u8> = (0..9 << 20).map(|i| (i % 251) as u8).collect();
        let fill = [0x11u8, 0x22, 0x33, 0x44];
        let filled: Vec<u8> = (0..9 << 20).map(|i| fill[i % 4]).collect();
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&raw);
        write_zeroes(&mut hasher, 5 << 20);
        hasher.update(&filled);
        let crc = hasher.finalize();

        let bytes = image(
            BLOCK,
            &[
                (RAW, blocks(raw.len()), &raw),
                (DONT_CARE, blocks(5 << 20), &[]),
                (FILL, blocks(filled.len()), &fill),
            ],
            Some(crc),
        );
        let expanded = raw.len() + (5 << 20) + filled.len();
        let mut input = Cursor::new(bytes.clone());
        assert_eq!(plan(&mut input, bytes.len() as u64).unwrap().len, expanded as u64);

        let mut out = Cursor::new(vec![0xa5u8; expanded]);
        flash(&mut input, bytes.len() as u64, &mut out, &mut |_, _| {}).unwrap();
        let written = out.get_ref();
        assert!(written[..raw.len()] == raw[..], "the raw chunk, split over three buffers");
        assert!(
            written[raw.len()..raw.len() + (5 << 20)].iter().all(|b| *b == 0xa5),
            "the hole is left as the device held it"
        );
        assert!(written[raw.len() + (5 << 20)..] == filled[..], "the fill, split over three buffers");
    }

    #[test]
    fn pipelined_chunks_preserve_holes_and_check_crc() {
        let block = 4096;
        let raw: Vec<u8> = (0..block as usize * 2).map(|i| (i % 251) as u8).collect();
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&raw);
        write_zeroes(&mut hasher, 8192);
        hasher.update(&[1, 2, 3, 4].repeat(2048));
        let crc = hasher.finalize();
        let bytes = image(block, &[(RAW, 2, &raw), (DONT_CARE, 2, &[]), (FILL, 2, &[1, 2, 3, 4])], Some(crc));
        let mut input = Cursor::new(bytes.clone());
        assert_eq!(plan(&mut input, bytes.len() as u64).unwrap().len, 6 * block as u64);
        let mut out = Cursor::new(vec![0xa5; 6 * block as usize]);
        flash(&mut input, bytes.len() as u64, &mut out, &mut |_, _| {}).unwrap();
        assert_eq!(&out.get_ref()[..raw.len()], raw);
        assert!(out.get_ref()[raw.len()..raw.len() + 8192].iter().all(|b| *b == 0xa5));
        assert!(out.get_ref()[raw.len() + 8192..].chunks(4).all(|b| b == [1, 2, 3, 4]));
    }

    #[test]
    fn invalid_format_is_rejected_before_output_is_changed() {
        let raw = [7u8; 4096];
        let mut bytes = image(4096, &[(RAW, 1, &raw)], None);
        bytes.truncate(100);
        let mut input = Cursor::new(bytes.clone());
        assert!(plan(&mut input, 100).is_err());
        let mut out = Cursor::new(vec![0xa5; 4096]);
        assert!(flash(&mut input, 100, &mut out, &mut |_, _| {}).is_err());
        assert_eq!(out.get_ref(), &vec![0xa5; 4096]);
    }

    #[test]
    fn longer_headers_and_trailing_padding_are_read_as_the_format_says() {
        let raw: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
        let bytes = image_with_extensions(4096, &[(RAW, 2, &raw), (FILL, 1, &[9, 8, 7, 6])], 32, 16);
        let expanded = 3 * 4096u64;
        let mut input = Cursor::new(bytes.clone());
        assert_eq!(plan(&mut input, bytes.len() as u64).unwrap().len, expanded, "the extension is skipped, not counted");
        let mut out = Cursor::new(vec![0xa5u8; expanded as usize]);
        flash(&mut input, bytes.len() as u64, &mut out, &mut |_, _| {}).unwrap();
        assert_eq!(&out.get_ref()[..raw.len()], &raw[..]);
        assert!(out.get_ref()[raw.len()..].chunks(4).all(|b| b == [9, 8, 7, 6]));

        // A tool that pads its file to a round size is still a sparse image.
        let mut padded = image(4096, &[(RAW, 1, &raw[..4096])], None);
        padded.extend(std::iter::repeat_n(0u8, 512));
        let mut input = Cursor::new(padded.clone());
        assert_eq!(plan(&mut input, padded.len() as u64).unwrap().len, 4096u64, "the padding is not part of the image");
        let mut out = Cursor::new(vec![0xa5u8; 4096]);
        flash(&mut input, padded.len() as u64, &mut out, &mut |_, _| {}).unwrap();
        assert_eq!(out.get_ref(), &raw[..4096]);
    }

    #[test]
    fn wrong_crc_stops_the_write() {
        let raw = [7u8; 4096];
        let bytes = image(4096, &[(RAW, 1, &raw)], Some(0));
        let mut input = Cursor::new(bytes.clone());
        let mut out = Cursor::new(vec![0; 4096]);
        let result = flash(&mut input, bytes.len() as u64, &mut out, &mut |_, _| {});
        assert!(result.is_err(), "bad CRC was accepted: {result:?}");
    }

    /// A sparse image whose headers and chunks are longer than the smallest
    /// ones Android writes, which a reader that only knew those would misread.
    fn image_with_extensions(block: u32, chunks: &[(u16, u32, &[u8])], file_header: u16, chunk_header: u16) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(MAGIC);
        for n in [1u16, 0, file_header, chunk_header] {
            out.extend(n.to_le_bytes());
        }
        for n in [block, chunks.iter().map(|c| c.1).sum(), chunks.len() as u32, 0] {
            out.extend(n.to_le_bytes());
        }
        out.resize(file_header as usize, 0);
        for (kind, blocks, data) in chunks {
            out.extend(kind.to_le_bytes());
            out.extend(0u16.to_le_bytes());
            out.extend(blocks.to_le_bytes());
            out.extend(((chunk_header as usize + data.len()) as u32).to_le_bytes());
            out.extend(std::iter::repeat_n(0u8, chunk_header as usize - CHUNK_HEADER as usize));
            out.extend(*data);
        }
        out
    }

    #[test]
    fn a_sparse_image_whose_expanded_bytes_are_the_geometry_is_told_apart() {
        let magic = 0x616c_4467u32.to_le_bytes();

        let mut holding = vec![0u8; 8192];
        holding[4096..4100].copy_from_slice(&magic);
        let bytes = image(4096, &[(RAW, 2, &holding)], None);
        assert!(expands_to(&mut Cursor::new(bytes.clone()), 4096, magic).unwrap(), "a raw chunk holding the geometry");

        let plain = vec![0u8; 8192];
        let bytes = image(4096, &[(RAW, 2, &plain)], None);
        assert!(!expands_to(&mut Cursor::new(bytes.clone()), 4096, magic).unwrap(), "zeros where the geometry would be");

        // A block-aligned fill of the geometry's own bytes.
        let bytes = image(4096, &[(FILL, 2, &magic)], None);
        assert!(expands_to(&mut Cursor::new(bytes.clone()), 4096, magic).unwrap(), "a fill pattern the geometry is");

        // A hole reads as whatever the device holds, which an image cannot say.
        let bytes = image(4096, &[(DONT_CARE, 2, &[])], None);
        assert!(!expands_to(&mut Cursor::new(bytes.clone()), 4096, magic).unwrap(), "a hole is not the geometry");

        // Anything that is not a sparse image at all.
        assert!(expands_to(&mut Cursor::new(vec![0u8; 8192]), 4096, magic).is_err());
    }

    #[test]
    fn a_header_naming_more_chunks_than_the_file_holds_is_refused() {
        let raw = [7u8; 4096];
        let mut bytes = image(4096, &[(RAW, 1, &raw)], None);
        // The chunk count is the third field of the header, at byte 20.
        bytes[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
        let mut input = Cursor::new(bytes.clone());
        let error = plan(&mut input, bytes.len() as u64).unwrap_err().to_string();
        assert!(error.contains("do not fit"), "{error}");
    }

    #[test]
    fn multiple_holes_end_at_expanded_length() {
        let bytes = image(4096, &[(DONT_CARE, 2, &[]), (DONT_CARE, 3, &[])], Some(0));
        let mut input = Cursor::new(bytes.clone());
        let mut out = Cursor::new(vec![0xa5; 20480]);
        let result = flash(&mut input, bytes.len() as u64, &mut out, &mut |_, _| {});
        assert!(result.is_err(), "bad CRC was accepted: {result:?}");
        assert!(out.get_ref().iter().all(|b| *b == 0xa5));
    }
}
