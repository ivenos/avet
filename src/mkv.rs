use anyhow::{ensure, Context, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

const EBML_HEADER: u64 = 0x1A45_DFA3;
const SEGMENT: u64 = 0x1853_8067;
const TRACKS: u64 = 0x1654_AE6B;
const CLUSTER: u64 = 0x1F43_B675;
const TRACK_ENTRY: u64 = 0xAE;
const CODEC_ID: u64 = 0x86;
const CODEC_PRIVATE: u64 = 0x63A2;
const VOID: u8 = 0xEC;

/// An element within a buffer; `size` is None for the unknown-size marker.
struct Element {
    id: u64,
    start: usize,
    data: usize,
    size: Option<u64>,
}

/// Vint length from the leading zero bits of its first byte.
fn vint_len(first: u8) -> Option<usize> {
    (first != 0).then(|| first.leading_zeros() as usize + 1)
}

fn read_element(buf: &[u8], start: usize) -> Option<Element> {
    let id_len = vint_len(*buf.get(start)?)?;
    let id = buf.get(start..start + id_len)?.iter().fold(0u64, |v, &b| v << 8 | u64::from(b));
    let size_pos = start + id_len;
    let size_len = vint_len(*buf.get(size_pos)?)?;
    let bytes = buf.get(size_pos..size_pos + size_len)?;
    let mask = if size_len == 8 { 0 } else { 0xFFu8 >> size_len };
    let value = bytes[1..].iter().fold(u64::from(bytes[0] & mask), |v, &b| v << 8 | u64::from(b));
    let unknown = value == (1u64 << (7 * size_len)) - 1;
    Some(Element { id, start, data: size_pos + size_len, size: (!unknown).then_some(value) })
}

fn children(buf: &[u8]) -> Vec<Element> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(el) = read_element(buf, pos) {
        let Some(end) = el.size.and_then(|s| el.data.checked_add(usize::try_from(s).ok()?)) else { break };
        if end > buf.len() {
            break;
        }
        pos = end;
        out.push(el);
    }
    out
}

fn write_vint(value: u64, len: usize, out: &mut Vec<u8>) {
    let marked = value | 1u64 << (7 * len);
    out.extend_from_slice(&marked.to_be_bytes()[8 - len..]);
}

/// The element rewritten to hold `data`, padded with a Void to its old length, so no
/// offset after it moves. None when the Void does not fit.
fn shrink_element(old: &[u8], el: &Element, data: &[u8]) -> Option<Vec<u8>> {
    let size_len = el.data - el.start - vint_len(old[el.start])?;
    let mut out = old[el.start..el.data - size_len].to_vec();
    write_vint(data.len() as u64, size_len, &mut out);
    out.extend_from_slice(data);

    let end = el.data + usize::try_from(el.size?).ok()?;
    let void_total = (end - el.start).checked_sub(out.len())?;
    let void_size_len = (1..=8).find(|&n| {
        void_total > n && ((void_total - 1 - n) as u64) < (1u64 << (7 * n)) - 1
    })?;
    out.push(VOID);
    write_vint((void_total - 1 - void_size_len) as u64, void_size_len, &mut out);
    out.resize(end - el.start, 0);
    Some(out)
}

/// Drops per-frame metadata OBUs from every AV1 track's av1C in `tracks`, in place.
fn trim_tracks(tracks: &mut [u8]) -> bool {
    let mut changed = false;
    for entry in children(tracks) {
        let Some(len) = entry.size.and_then(|s| usize::try_from(s).ok()) else { continue };
        if entry.id != TRACK_ENTRY {
            continue;
        }
        let body = &tracks[entry.data..entry.data + len];
        let fields = children(body);
        let is_av1 = fields.iter().any(|f| {
            f.id == CODEC_ID && f.size.is_some_and(|s| body.get(f.data..f.data + s as usize) == Some(&b"V_AV1"[..]))
        });
        let Some(private) = fields.iter().find(|f| f.id == CODEC_PRIVATE) else { continue };
        if !is_av1 {
            continue;
        }
        let Some(av1c) = private.size.and_then(|s| body.get(private.data..private.data + s as usize)) else { continue };
        let Some(trimmed) = crate::av1::av1c_without_frame_metadata(av1c) else { continue };
        let Some(bytes) = shrink_element(body, private, &trimmed) else { continue };

        let at = entry.data + private.start;
        tracks[at..at + bytes.len()].copy_from_slice(&bytes);
        changed = true;
    }
    changed
}

/// mkvmerge copies the metadata OBUs in front of the first frame into av1C, HDR10+ and
/// Dolby Vision included. Returns whether anything was removed.
pub fn trim_av1_codec_private(path: &Path) -> Result<bool> {
    let mut f = File::options().read(true).write(true).open(path)
        .with_context(|| format!("open {}", path.display()))?;
    let file_len = f.metadata()?.len();

    // Enough for any element header: an 4-byte ID and an 8-byte size.
    let mut head = [0u8; 12];
    let mut read_header = |f: &mut File, pos: u64| -> Result<Element> {
        f.seek(SeekFrom::Start(pos))?;
        let n = f.read(&mut head)?;
        read_element(&head[..n], 0).with_context(|| format!("no EBML element at byte {pos}"))
    };

    let ebml = read_header(&mut f, 0)?;
    ensure!(ebml.id == EBML_HEADER, "{} is not a Matroska file", path.display());
    let segment_pos = ebml.data as u64 + ebml.size.context("EBML header of unknown size")?;
    let segment = read_header(&mut f, segment_pos)?;
    ensure!(segment.id == SEGMENT, "no Segment after the EBML header");
    let segment_data = segment_pos + segment.data as u64;
    let segment_end = segment.size.map_or(file_len, |s| segment_data + s);

    let mut pos = segment_data;
    while pos < segment_end {
        let el = read_header(&mut f, pos)?;
        if el.id == CLUSTER {
            break;
        }
        let size = el.size.context("top-level element of unknown size before the first cluster")?;
        let data_pos = pos + el.data as u64;
        // A truncated file would otherwise allocate whatever its size field claims.
        ensure!(
            data_pos.checked_add(size).is_some_and(|end| end <= file_len),
            "element at byte {pos} runs past the end of {}", path.display()
        );
        if el.id == TRACKS {
            let mut tracks = vec![0u8; usize::try_from(size)?];
            f.seek(SeekFrom::Start(data_pos))?;
            f.read_exact(&mut tracks)?;
            if !trim_tracks(&mut tracks) {
                return Ok(false);
            }
            f.seek(SeekFrom::Start(data_pos))?;
            f.write_all(&tracks)?;
            f.sync_all()?;
            return Ok(true);
        }
        pos += el.data as u64 + size;
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(id: &[u8], data: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        write_vint(data.len() as u64, 2, &mut out);
        out.extend_from_slice(data);
        out
    }

    fn obu(kind: u8, payload: &[u8]) -> Vec<u8> {
        let size = match payload.len() {
            n @ 0..=127 => vec![n as u8],
            n => vec![(n & 0x7f) as u8 | 0x80, (n >> 7) as u8],
        };
        [vec![(kind << 3) | 0x02], size, payload.to_vec()].concat()
    }

    #[test]
    fn a_tracks_size_past_the_end_of_the_file_is_an_error_not_an_allocation() {
        let file = [
            vec![0x1A, 0x45, 0xDF, 0xA3, 0x83, 0x42, 0x82, 0x88],
            vec![0x18, 0x53, 0x80, 0x67, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
            vec![0x16, 0x54, 0xAE, 0x6B, 0x01, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00],
        ].concat();

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("truncated.mkv");
        std::fs::write(&path, &file).unwrap();
        assert!(trim_av1_codec_private(&path).is_err());
    }

    #[test]
    fn av1c_shrinks_in_place_and_nothing_after_it_moves() {
        let seq = obu(1, &[0x00, 0x00, 0x00]);
        let cll = obu(5, &[1, 0x03, 0xe8, 0x01, 0x90, 0x80]);
        let rpu = obu(5, &[4, 0xB5, 0x00, 0x3B, 0x00, 0x00, 0x08, 0x00, 0x37, 0xCD, 0x08, 0x80]);
        let av1c = [vec![0x81, 0x05, 0x4c, 0x00], seq.clone(), cll.clone(), rpu].concat();

        let entry = element(&[0xAE], &[
            element(&[0x86], b"V_AV1"),
            element(&[0x63, 0xA2], &av1c),
            element(&[0xB0], &[0x05, 0x00]),
        ].concat());
        let tracks = element(&[0x16, 0x54, 0xAE, 0x6B], &entry);
        let cluster = element(&[0x1F, 0x43, 0xB6, 0x75], &[0xE7, 0x81, 0x00]);
        let segment = element(&[0x18, 0x53, 0x80, 0x67], &[element(&[0x15, 0x49, 0xA9, 0x66], &[]), tracks, cluster.clone()].concat());
        let file = [element(&[0x1A, 0x45, 0xDF, 0xA3], &[0x42, 0x82, 0x88]), segment].concat();

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("muxed.mkv");
        std::fs::write(&path, &file).unwrap();

        assert!(trim_av1_codec_private(&path).unwrap());
        let out = std::fs::read(&path).unwrap();
        assert_eq!(out.len(), file.len());
        assert!(out.ends_with(&cluster));

        let private_at = out.windows(2).position(|w| w == [0x63, 0xA2]).unwrap();
        let private = read_element(&out, private_at).unwrap();
        let data = &out[private.data..private.data + private.size.unwrap() as usize];
        assert_eq!(data, [vec![0x81, 0x05, 0x4c, 0x00], seq, cll].concat());

        let void = read_element(&out, private.data + data.len()).unwrap();
        assert_eq!(void.id, u64::from(VOID));
        let pixel_width = read_element(&out, void.data + void.size.unwrap() as usize).unwrap();
        assert_eq!(pixel_width.id, 0xB0);

        assert!(!trim_av1_codec_private(&path).unwrap());
    }

    fn muxed(codec_id: &[u8], av1c: &[u8]) -> Vec<u8> {
        let entry = element(&[0xAE], &[element(&[0x86], codec_id), element(&[0x63, 0xA2], av1c)].concat());
        let segment = element(&[0x18, 0x53, 0x80, 0x67], &element(&[0x16, 0x54, 0xAE, 0x6B], &entry));
        [element(&[0x1A, 0x45, 0xDF, 0xA3], &[0x42, 0x82, 0x88]), segment].concat()
    }

    #[test]
    fn a_dolby_vision_sized_removal_gets_a_two_byte_void_and_other_codecs_are_left_alone() {
        let seq = obu(1, &[0x00, 0x00, 0x00]);
        let rpu = obu(5, &[vec![4, 0xB5, 0x00, 0x3B], vec![0x5A; 160], vec![0x80]].concat());
        let av1c = [vec![0x81, 0x05, 0x4c, 0x00], seq.clone(), rpu].concat();
        let dir = tempfile::TempDir::new().unwrap();

        let path = dir.path().join("av1.mkv");
        let file = muxed(b"V_AV1", &av1c);
        std::fs::write(&path, &file).unwrap();
        assert!(trim_av1_codec_private(&path).unwrap());
        let out = std::fs::read(&path).unwrap();
        assert_eq!(out.len(), file.len());
        let private_at = out.windows(2).position(|w| w == [0x63, 0xA2]).unwrap();
        let private = read_element(&out, private_at).unwrap();
        let void = read_element(&out, private.data + private.size.unwrap() as usize).unwrap();
        assert_eq!((void.id, void.data - void.start), (u64::from(VOID), 3));
        assert_eq!(void.data + void.size.unwrap() as usize, out.len());

        let path = dir.path().join("hevc.mkv");
        let file = muxed(b"V_MPEGH/ISO/HEVC", &av1c);
        std::fs::write(&path, &file).unwrap();
        assert!(!trim_av1_codec_private(&path).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), file);
    }

    use proptest::prelude::*;

    fn sized_element(id: &[u8], data: &[u8], width: usize) -> Vec<u8> {
        let mut out = id.to_vec();
        write_vint(data.len() as u64, width, &mut out);
        out.extend_from_slice(data);
        out
    }

    proptest! {
        #[test]
        fn trimming_touches_only_the_codec_private(
            kinds in prop::collection::vec((0u8..6, prop::collection::vec(any::<u8>(), 1..300)), 0..6),
            before in prop::collection::vec(any::<u8>(), 0..50),
            after in prop::collection::vec(any::<u8>(), 0..50),
            width in 2usize..=8,
        ) {
            // Metadata types 1 and 2 stay; 3, 4 and 5 go; kind 0 is a sequence header.
            let mut av1c = vec![0x81, 0x05, 0x4c, 0x00];
            let mut kept = av1c.clone();
            for (kind, body) in &kinds {
                let o = if *kind == 0 { obu(1, body) } else { obu(5, &[vec![*kind], body.clone()].concat()) };
                if matches!(kind, 0..=2) { kept.extend_from_slice(&o); }
                av1c.extend(o);
            }
            let private = sized_element(&[0x63, 0xA2], &av1c, width);
            let entry = element(&[0xAE], &[element(&[0x86], b"V_AV1"), element(&[0xEA], &before), private.clone(), element(&[0xEB], &after)].concat());
            let segment = element(&[0x18, 0x53, 0x80, 0x67], &[element(&[0x16, 0x54, 0xAE, 0x6B], &entry), element(&[0x1F, 0x43, 0xB6, 0x75], &[0xA3, 0x80])].concat());
            let file = [element(&[0x1A, 0x45, 0xDF, 0xA3], &[0x42, 0x82, 0x88]), segment].concat();

            let dir = tempfile::TempDir::new().unwrap();
            let path = dir.path().join("t.mkv");
            std::fs::write(&path, &file).unwrap();
            let changed = trim_av1_codec_private(&path).unwrap();
            let out = std::fs::read(&path).unwrap();

            prop_assert_eq!(out.len(), file.len());
            let at = file.windows(private.len()).position(|w| w == private.as_slice()).unwrap();
            prop_assert_eq!(&out[..at], &file[..at]);
            prop_assert_eq!(&out[at + private.len()..], &file[at + private.len()..]);
            prop_assert_eq!(changed, kept.len() != av1c.len());

            let el = read_element(&out, at).unwrap();
            prop_assert_eq!(el.id, CODEC_PRIVATE);
            let data_end = el.data + el.size.unwrap() as usize;
            prop_assert_eq!(&out[el.data..data_end], kept.as_slice());
            if changed {
                let void = read_element(&out, data_end).unwrap();
                prop_assert_eq!(void.id, u64::from(VOID));
                prop_assert_eq!(void.data + void.size.unwrap() as usize, at + private.len());
            }
        }
    }

    #[test]
    fn vints_are_written_at_the_requested_width() {
        let mut out = Vec::new();
        write_vint(5, 2, &mut out);
        assert_eq!(out, [0x40, 0x05]);
        let el = read_element(&[0xEC, 0x40, 0x05], 0).unwrap();
        assert_eq!((el.id, el.data, el.size), (0xEC, 3, Some(5)));
        let unknown = read_element(&[0x18, 0x53, 0x80, 0x67, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF], 0).unwrap();
        assert_eq!(unknown.size, None);
    }
}
