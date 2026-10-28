//! Minimal RIFF/WAVE chunk handling. hound ignores `cue ` chunks, so reels
//! are read and rewritten here at the chunk level.

use anyhow::{bail, Context, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub id: [u8; 4],
    pub data: Vec<u8>,
}

/// Where a chunk sits in a file, without its contents.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChunkInfo {
    pub id: [u8; 4],
    /// Offset of the chunk's data, after the 8-byte header.
    pub offset: u64,
    pub size: u32,
}

/// Split a whole WAVE file into its chunks.
pub fn parse(bytes: &[u8]) -> Result<Vec<Chunk>> {
    check_header(
        bytes
            .get(..12)
            .context("file is shorter than a RIFF header")?,
    )?;
    let mut chunks = Vec::new();
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let id: [u8; 4] = bytes[pos..pos + 4].try_into().unwrap();
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let start = pos + 8;
        // Writers that crash mid-recording leave a size that runs past the
        // end of the file, so take what is there.
        let end = (start + size).min(bytes.len());
        chunks.push(Chunk {
            id,
            data: bytes[start..end].to_vec(),
        });
        pos = start + size + (size & 1);
    }
    Ok(chunks)
}

/// Join chunks back into a WAVE file.
pub fn serialize(chunks: &[Chunk]) -> Vec<u8> {
    let body: usize = chunks
        .iter()
        .map(|c| 8 + c.data.len() + (c.data.len() & 1))
        .sum();
    let mut out = Vec::with_capacity(12 + body);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((4 + body) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    for chunk in chunks {
        out.extend_from_slice(&chunk.id);
        out.extend_from_slice(&(chunk.data.len() as u32).to_le_bytes());
        out.extend_from_slice(&chunk.data);
        if chunk.data.len() & 1 == 1 {
            out.push(0);
        }
    }
    out
}

/// List the chunks in a file by reading only their headers, so a library of
/// full reels can be scanned without reading the audio.
pub fn scan_file(path: &Path) -> Result<Vec<ChunkInfo>> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut header = [0u8; 12];
    file.read_exact(&mut header)
        .context("file is shorter than a RIFF header")?;
    check_header(&header)?;
    let mut infos = Vec::new();
    let mut pos = 12u64;
    while pos + 8 <= len {
        file.seek(SeekFrom::Start(pos))?;
        let mut head = [0u8; 8];
        file.read_exact(&mut head)?;
        let id: [u8; 4] = head[..4].try_into().unwrap();
        let declared = u32::from_le_bytes(head[4..].try_into().unwrap());
        let size = (declared as u64).min(len - pos - 8) as u32;
        infos.push(ChunkInfo {
            id,
            offset: pos + 8,
            size,
        });
        pos += 8 + declared as u64 + (declared as u64 & 1);
    }
    Ok(infos)
}

/// What [`scan_source`] reads from a WAV file.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceScan {
    pub format: Format,
    /// The size of the audio, in bytes.
    pub data_bytes: u64,
    /// The file's [`source_markers`], at its own rate.
    pub markers: Vec<u32>,
}

/// A WAV file's format, audio size and [`source_markers`], read from the
/// chunk headers and the small chunks only, so the audio stays on disk.
pub fn scan_source(path: &Path) -> Result<SourceScan> {
    let chunks = scan_file(path)?;
    let find = |id: &[u8; 4]| chunks.iter().find(|c| &c.id == id);
    let format = Format::parse(&read_chunk(path, find(b"fmt ").context("no fmt chunk")?)?)?;
    let cue = find(b"cue ").map(|c| read_chunk(path, c)).transpose()?;
    let lists = chunks
        .iter()
        .filter(|c| &c.id == b"LIST")
        .map(|c| read_chunk(path, c))
        .collect::<Result<Vec<_>>>()?;
    let lists: Vec<&[u8]> = lists.iter().map(Vec::as_slice).collect();
    Ok(SourceScan {
        format,
        data_bytes: find(b"data").map_or(0, |c| c.size as u64),
        markers: source_markers(cue.as_deref(), &lists),
    })
}

pub fn read_chunk(path: &Path, info: &ChunkInfo) -> Result<Vec<u8>> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(info.offset))?;
    let mut data = vec![0u8; info.size as usize];
    file.read_exact(&mut data)?;
    Ok(data)
}

fn check_header(header: &[u8]) -> Result<()> {
    if &header[..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        bail!("not a RIFF/WAVE file");
    }
    Ok(())
}

/// Encode splice markers as a `cue ` chunk, in the layout the Morphagene
/// writes: one point per marker, dwPosition and dwSampleOffset both set.
pub fn encode_cues(markers: &[u32]) -> Chunk {
    let mut data = Vec::with_capacity(4 + markers.len() * 24);
    data.extend_from_slice(&(markers.len() as u32).to_le_bytes());
    for (i, &position) in markers.iter().enumerate() {
        data.extend_from_slice(&(i as u32 + 1).to_le_bytes()); // dwName
        data.extend_from_slice(&position.to_le_bytes()); // dwPosition
        data.extend_from_slice(b"data"); // fccChunk
        data.extend_from_slice(&0u32.to_le_bytes()); // dwChunkStart
        data.extend_from_slice(&0u32.to_le_bytes()); // dwBlockStart
        data.extend_from_slice(&position.to_le_bytes()); // dwSampleOffset
    }
    Chunk { id: *b"cue ", data }
}

/// Read splice markers from a `cue ` chunk, sorted and de-duplicated.
pub fn decode_cues(data: &[u8]) -> Vec<u32> {
    let mut markers: Vec<u32> = cue_points(data).into_iter().map(|(_, at)| at).collect();
    markers.sort_unstable();
    markers.dedup();
    markers
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// The id and dwSampleOffset of each point in a `cue ` chunk, in file order.
pub fn cue_points(data: &[u8]) -> Vec<(u32, u32)> {
    let count = data.get(..4).map_or(0, |b| le32(b) as usize);
    data[4.min(data.len())..]
        .as_chunks::<24>()
        .0
        .iter()
        .take(count)
        .map(|p| (le32(&p[..4]), le32(&p[20..])))
        .collect()
}

/// The cue id and length in samples of each `ltxt` region in a `LIST`
/// chunk's data. A list that isn't `adtl` has none.
pub fn ltxt_regions(list: &[u8]) -> Vec<(u32, u32)> {
    let Some(mut rest) = list.strip_prefix(b"adtl") else {
        return Vec::new();
    };
    let mut regions = Vec::new();
    while rest.len() >= 8 {
        let size = le32(&rest[4..8]) as usize;
        let Some(body) = rest.get(8..8 + size) else {
            break;
        };
        if &rest[..4] == b"ltxt" && body.len() >= 8 {
            regions.push((le32(&body[..4]), le32(&body[4..8])));
        }
        // Subchunks are padded to an even length.
        rest = rest.get(8 + size + (size & 1)..).unwrap_or_default();
    }
    regions
}

/// The splice points a file from other software carries, at its own sample
/// rate: its cue points and the end of each `ltxt` region, sorted. RX and
/// Pro Tools mark a region with a cue point and an `ltxt` length, and the
/// module reads only cue points, so a reel would lose the region ends. Not
/// for reading a reel already on the card: there only the cue points count.
pub fn source_markers(cue: Option<&[u8]>, lists: &[&[u8]]) -> Vec<u32> {
    let points = cue.map(cue_points).unwrap_or_default();
    let mut markers: Vec<u32> = points.iter().map(|&(_, at)| at).collect();
    for list in lists {
        for (id, length) in ltxt_regions(list) {
            if let Some(&(_, start)) = points.iter().find(|&&(p, _)| p == id) {
                markers.push(start.saturating_add(length));
            }
        }
    }
    markers.sort_unstable();
    markers.dedup();
    markers
}

/// The parts of a `fmt ` chunk a reel cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    pub float: bool,
    pub channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
}

impl Format {
    pub fn parse(fmt: &[u8]) -> Result<Format> {
        if fmt.len() < 16 {
            bail!("fmt chunk is too short");
        }
        let le16 = |i: usize| u16::from_le_bytes([fmt[i], fmt[i + 1]]);
        let mut tag = le16(0);
        if tag == 0xFFFE {
            // WAVE_FORMAT_EXTENSIBLE: the real format is the first two bytes
            // of the sub-format GUID.
            tag = fmt
                .get(24..26)
                .map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
        }
        let float = match tag {
            1 => false,
            3 => true,
            other => bail!("unsupported WAVE format tag {other:#06x}"),
        };
        let format = Format {
            float,
            channels: le16(2),
            sample_rate: u32::from_le_bytes(fmt[4..8].try_into().unwrap()),
            bits_per_sample: le16(14),
        };
        // A zero here would make the frame size or the resample ratio zero.
        if format.channels == 0 {
            bail!("fmt chunk declares no channels");
        }
        if format.sample_rate == 0 {
            bail!("fmt chunk declares a sample rate of 0");
        }
        if format.bits_per_sample < 8 || !format.bits_per_sample.is_multiple_of(8) {
            bail!("unsupported bit depth {}", format.bits_per_sample);
        }
        Ok(format)
    }

    pub fn frame_bytes(&self) -> usize {
        self.channels as usize * (self.bits_per_sample as usize / 8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
        if body.len() % 2 == 1 {
            out.push(0);
        }
        out
    }

    #[test]
    fn region_ends_count_as_source_markers() {
        // Cue 1 at 100 starts a 50-sample region; cue 2 at 400 is a point.
        let cue = encode_cues(&[100, 400]);
        let mut ltxt = Vec::new();
        ltxt.extend_from_slice(&1u32.to_le_bytes());
        ltxt.extend_from_slice(&50u32.to_le_bytes());
        ltxt.extend_from_slice(b"rgn \0\0\0\0\0\0\0\0");
        let mut list = b"adtl".to_vec();
        // An odd-length label before it checks the padding.
        let mut labl = 2u32.to_le_bytes().to_vec();
        labl.extend_from_slice(b"hit\0\0");
        list.extend(sub(b"labl", &labl));
        list.extend(sub(b"ltxt", &ltxt));
        assert_eq!(ltxt_regions(&list), vec![(1, 50)]);
        assert_eq!(
            source_markers(Some(&cue.data), &[&list]),
            vec![100, 150, 400]
        );
        // Only cue points without the list, and nothing from other lists.
        assert_eq!(
            source_markers(Some(&cue.data), &[b"INFOISFT"]),
            vec![100, 400]
        );
        assert_eq!(decode_cues(&cue.data), vec![100, 400]);
        // A region whose cue is missing adds nothing.
        assert_eq!(source_markers(None, &[&list]), Vec::<u32>::new());
    }

    fn fmt_chunk(channels: u16, rate: u32, bits: u16) -> Vec<u8> {
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&1u16.to_le_bytes());
        fmt.extend_from_slice(&channels.to_le_bytes());
        fmt.extend_from_slice(&rate.to_le_bytes());
        fmt.extend_from_slice(&0u32.to_le_bytes());
        fmt.extend_from_slice(&0u16.to_le_bytes());
        fmt.extend_from_slice(&bits.to_le_bytes());
        fmt
    }

    #[test]
    fn format_rejects_degenerate_headers() {
        assert!(Format::parse(&fmt_chunk(2, 48_000, 16)).is_ok());
        assert!(Format::parse(&fmt_chunk(0, 48_000, 16)).is_err());
        assert!(Format::parse(&fmt_chunk(2, 0, 16)).is_err());
        assert!(Format::parse(&fmt_chunk(2, 48_000, 4)).is_err());
        assert!(Format::parse(&fmt_chunk(2, 48_000, 12)).is_err());
    }

    #[test]
    fn cue_round_trip() {
        let chunk = encode_cues(&[960, 1560]);
        assert_eq!(chunk.data.len(), 4 + 2 * 24);
        assert_eq!(decode_cues(&chunk.data), vec![960, 1560]);
    }

    #[test]
    fn decode_cues_sorts_and_dedups() {
        let mut chunk = encode_cues(&[500, 100, 500]);
        chunk.data[0] = 3;
        assert_eq!(decode_cues(&chunk.data), vec![100, 500]);
    }

    #[test]
    fn serialize_pads_odd_chunks() {
        let chunks = vec![
            Chunk {
                id: *b"abcd",
                data: vec![1, 2, 3],
            },
            Chunk {
                id: *b"efgh",
                data: vec![4],
            },
        ];
        let bytes = serialize(&chunks);
        assert_eq!(bytes.len(), 12 + 8 + 4 + 8 + 2);
        assert_eq!(parse(&bytes).unwrap(), chunks);
    }
}
