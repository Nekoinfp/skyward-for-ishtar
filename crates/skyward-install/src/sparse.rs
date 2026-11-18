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
                let mut pattern = [0; 4];
                input.read_exact(&mut pattern)?;
                Chunk::Fill { pattern, len: chunk_len }
            }
            DONT_CARE if payload == 0 => Chunk::Skip { len: chunk_len },
            CRC32 if payload == 4 && blocks == 0 => {
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
    ensure!(at == file_size, "sparse image has trailing bytes or a truncated header extension");
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
    fn wrong_crc_stops_the_write() {
        let raw = [7u8; 4096];
        let bytes = image(4096, &[(RAW, 1, &raw)], Some(0));
        let mut input = Cursor::new(bytes.clone());
        let mut out = Cursor::new(vec![0; 4096]);
        let result = flash(&mut input, bytes.len() as u64, &mut out, &mut |_, _| {});
        assert!(result.is_err(), "bad CRC was accepted: {result:?}");
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
