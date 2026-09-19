use anyhow::{bail, ensure, Context, Result};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const OBU_SEQUENCE_HEADER: u8 = 1;
const OBU_FRAME_HEADER: u8 = 3;
const OBU_METADATA: u8 = 5;
const OBU_FRAME: u8 = 6;
const METADATA_TYPE_HDR_CLL: u8 = 1;
const METADATA_TYPE_HDR_MDCV: u8 = 2;
const METADATA_TYPE_ITUT_T35: u8 = 4;

struct Obu<'a> {
    kind: u8,
    payload: &'a [u8],
    raw: &'a [u8],
}

fn read_leb128(buf: &[u8], mut pos: usize) -> Option<(u64, usize)> {
    let mut value = 0u64;
    for i in 0..8 {
        let byte = *buf.get(pos)?;
        pos += 1;
        value |= u64::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            return Some((value, pos));
        }
    }
    None
}

fn write_leb128(mut value: usize, out: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// OBUs of one temporal unit; every OBU has to carry its size, as in IVF and Matroska.
fn parse_obus(tu: &[u8]) -> Result<Vec<Obu<'_>>> {
    let mut obus = Vec::new();
    let mut pos = 0;
    while pos < tu.len() {
        let header = tu[pos];
        ensure!(header & 0x02 != 0, "OBU without a size field");
        let extension = usize::from(header & 0x04 != 0);
        let (size, start) = read_leb128(tu, pos + 1 + extension).context("truncated OBU header")?;
        let end = usize::try_from(size).ok().and_then(|s| start.checked_add(s))
            .filter(|&e| e <= tu.len())
            .context("OBU runs past the end of its temporal unit")?;
        obus.push(Obu { kind: (header >> 3) & 0x0f, payload: &tu[start..end], raw: &tu[pos..end] });
        pos = end;
    }
    Ok(obus)
}

/// First bits of the uncompressed header: show_existing_frame, frame_type (2), show_frame.
fn is_shown_frame(obu: &Obu, reduced_still_picture_header: bool) -> bool {
    if !matches!(obu.kind, OBU_FRAME | OBU_FRAME_HEADER) {
        return false;
    }
    reduced_still_picture_header || obu.payload.first().is_some_and(|b| b & 0x90 != 0)
}

fn write_t35_metadata_obu(message: &[u8], out: &mut Vec<u8>) {
    let payload_len = 1 + message.len() + 1;
    out.push((OBU_METADATA << 3) | 0x02);
    write_leb128(payload_len, out);
    out.push(METADATA_TYPE_ITUT_T35);
    out.extend_from_slice(message);
    out.push(0x80);
}

fn read_ivf_header(r: &mut impl Read, path: &Path) -> Result<Vec<u8>> {
    let mut header = vec![0u8; 32];
    r.read_exact(&mut header).with_context(|| format!("read IVF header of {}", path.display()))?;
    ensure!(&header[..4] == b"DKIF", "{} is not an IVF file", path.display());
    let len = usize::from(u16::from_le_bytes([header[6], header[7]]));
    ensure!(len >= 32, "{} has an IVF header of {len} bytes", path.display());
    header.resize(len, 0);
    r.read_exact(&mut header[32..]).with_context(|| format!("read IVF header of {}", path.display()))?;
    Ok(header)
}

/// `(pts, temporal unit)`, or None at the end of the file.
fn read_ivf_frame(r: &mut impl Read) -> Result<Option<(u64, Vec<u8>)>> {
    let mut head = [0u8; 12];
    if r.read(&mut head[..1]).context("read IVF frame header")? == 0 {
        return Ok(None);
    }
    r.read_exact(&mut head[1..]).context("truncated IVF frame header")?;
    let size = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
    let pts = u64::from_le_bytes([head[4], head[5], head[6], head[7], head[8], head[9], head[10], head[11]]);
    // Growing into the read: a corrupt size field would otherwise allocate 4 GiB up front.
    let mut data = Vec::new();
    r.by_ref().take(size as u64).read_to_end(&mut data).context("read IVF frame")?;
    ensure!(data.len() == size, "truncated IVF frame: {} of {size} bytes", data.len());
    Ok(Some((pts, data)))
}

fn write_ivf_frame(w: &mut impl Write, pts: u64, data: &[u8]) -> Result<()> {
    let size = u32::try_from(data.len()).context("IVF frame larger than 4 GiB")?;
    w.write_all(&size.to_le_bytes())?;
    w.write_all(&pts.to_le_bytes())?;
    w.write_all(data)?;
    Ok(())
}

/// Puts `messages[n]`, ITU-T T.35 payloads from the country code on, in front of the
/// n-th shown frame. Hidden frames get none, as SVT-AV1 does with its own metadata.
pub fn insert_t35_metadata(path: &Path, messages: &[Vec<Vec<u8>>]) -> Result<()> {
    let tmp = path.with_extension("ivf.part");
    let mut r = BufReader::new(File::open(path).with_context(|| format!("open {}", path.display()))?);
    let mut w = BufWriter::new(File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?);

    let header = read_ivf_header(&mut r, path)?;
    w.write_all(&header)?;

    let mut reduced_still_picture_header = false;
    let mut shown = 0usize;
    let mut out = Vec::new();
    while let Some((pts, tu)) = read_ivf_frame(&mut r)? {
        out.clear();
        for obu in parse_obus(&tu)? {
            if obu.kind == OBU_SEQUENCE_HEADER {
                reduced_still_picture_header = obu.payload.first().is_some_and(|b| b & 0x08 != 0);
            }
            if is_shown_frame(&obu, reduced_still_picture_header) {
                let Some(frame) = messages.get(shown) else {
                    bail!("{} shows more frames than the {} decoded for it", path.display(), messages.len());
                };
                for message in frame {
                    write_t35_metadata_obu(message, &mut out);
                }
                shown += 1;
            }
            out.extend_from_slice(obu.raw);
        }
        write_ivf_frame(&mut w, pts, &out)?;
    }
    ensure!(
        shown == messages.len(),
        "{} shows {shown} frames, but {} were decoded for it", path.display(), messages.len()
    );

    let file = w.into_inner().map_err(|e| e.into_error())?;
    file.sync_all().with_context(|| format!("flush {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename {} to {}", tmp.display(), path.display()))
}

/// Joins IVF files into one with continuous timestamps; returns the frame count.
pub fn concat_ivf(inputs: &[PathBuf], output: &Path) -> Result<u64> {
    ensure!(!inputs.is_empty(), "no chunks to join");
    let mut w = BufWriter::new(File::create(output).with_context(|| format!("create {}", output.display()))?);
    let mut frames = 0u64;
    let mut first: Option<Vec<u8>> = None;
    for input in inputs {
        let mut r = BufReader::new(File::open(input).with_context(|| format!("open {}", input.display()))?);
        let header = read_ivf_header(&mut r, input)?;
        // Codec, frame size and time base; a mismatch would play back at the wrong rate.
        match &first {
            None => {
                w.write_all(&header)?;
                first = Some(header);
            }
            Some(f) => ensure!(
                f[8..24] == header[8..24],
                "{} was encoded with other stream parameters than {}", input.display(), inputs[0].display()
            ),
        }
        while let Some((_, frame)) = read_ivf_frame(&mut r).with_context(|| format!("read {}", input.display()))? {
            write_ivf_frame(&mut w, frames, &frame)?;
            frames += 1;
        }
    }

    let mut file = w.into_inner().map_err(|e| e.into_error())?;
    file.seek(SeekFrom::Start(24))?;
    file.write_all(&u32::try_from(frames).unwrap_or(u32::MAX).to_le_bytes())?;
    file.sync_all().with_context(|| format!("flush {}", output.display()))?;
    Ok(frames)
}

/// av1C with only the sequence header and HDR static metadata left, or None when it has
/// nothing else. Per-frame metadata does not belong there, HDR10+ forbids it outright.
pub fn av1c_without_frame_metadata(av1c: &[u8]) -> Option<Vec<u8>> {
    let obus = parse_obus(av1c.get(4..)?).ok()?;
    let keep = |obu: &Obu| {
        obu.kind == OBU_SEQUENCE_HEADER
            || (obu.kind == OBU_METADATA
                && matches!(obu.payload.first(), Some(&(METADATA_TYPE_HDR_CLL | METADATA_TYPE_HDR_MDCV))))
    };
    if obus.iter().all(keep) {
        return None;
    }
    let mut out = av1c[..4].to_vec();
    for obu in obus.iter().filter(|o| keep(o)) {
        out.extend_from_slice(obu.raw);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obu(kind: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![(kind << 3) | 0x02];
        write_leb128(payload.len(), &mut out);
        out.extend_from_slice(payload);
        out
    }

    fn ivf(tus: &[Vec<u8>]) -> Vec<u8> {
        let mut out = b"DKIF".to_vec();
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(b"AV01");
        out.extend_from_slice(&[0x80, 0x07, 0x38, 0x04]);
        out.extend_from_slice(&24u32.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&(tus.len() as u32).to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        for (pts, tu) in tus.iter().enumerate() {
            write_ivf_frame(&mut out, pts as u64, tu).unwrap();
        }
        out
    }

    fn frames(data: &[u8]) -> Vec<Vec<u8>> {
        let mut r = std::io::Cursor::new(data);
        read_ivf_header(&mut r, Path::new("t.ivf")).unwrap();
        std::iter::from_fn(|| read_ivf_frame(&mut r).unwrap().map(|(_, tu)| tu)).collect()
    }

    fn kinds(tu: &[u8]) -> Vec<u8> {
        parse_obus(tu).unwrap().iter().map(|o| o.kind).collect()
    }

    const TD: u8 = 2;

    #[test]
    fn a_frame_size_past_the_end_of_the_file_is_an_error_not_an_allocation() {
        let mut data = ivf(&[]);
        data.extend_from_slice(&u32::MAX.to_le_bytes());
        data.extend_from_slice(&0u64.to_le_bytes());
        data.extend_from_slice(b"four");

        let mut r = std::io::Cursor::new(data);
        read_ivf_header(&mut r, Path::new("t.ivf")).unwrap();
        assert!(read_ivf_frame(&mut r).is_err());
    }

    #[test]
    fn leb128_round_trips_multi_byte_sizes() {
        for value in [0usize, 127, 128, 300, 1 << 20] {
            let mut buf = Vec::new();
            write_leb128(value, &mut buf);
            assert_eq!(read_leb128(&buf, 0), Some((value as u64, buf.len())));
        }
    }

    #[test]
    fn metadata_lands_before_each_shown_frame_and_never_on_a_hidden_one() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("00001.ivf");
        // Key frame; hidden ARF plus shown frame; show_existing_frame of the ARF.
        let tus = [
            [obu(TD, &[]), obu(OBU_SEQUENCE_HEADER, &[0x00, 0x00]), obu(OBU_FRAME, &[0x10, 0xAA])].concat(),
            [obu(TD, &[]), obu(OBU_FRAME, &[0x20, 0xBB]), obu(OBU_FRAME, &[0x30, 0xCC])].concat(),
            [obu(TD, &[]), obu(OBU_FRAME_HEADER, &[0x80])].concat(),
        ];
        std::fs::write(&path, ivf(&tus)).unwrap();

        let big = vec![0xB5; 200];
        let messages = vec![vec![vec![0xB5, 1]], vec![big.clone(), vec![0xB5, 2]], vec![vec![0xB5, 3]]];
        insert_t35_metadata(&path, &messages).unwrap();

        let out = frames(&std::fs::read(&path).unwrap());
        assert_eq!(kinds(&out[0]), [TD, OBU_SEQUENCE_HEADER, OBU_METADATA, OBU_FRAME]);
        assert_eq!(kinds(&out[1]), [TD, OBU_FRAME, OBU_METADATA, OBU_METADATA, OBU_FRAME]);
        assert_eq!(kinds(&out[2]), [TD, OBU_METADATA, OBU_FRAME_HEADER]);

        let second = parse_obus(&out[1]).unwrap();
        assert_eq!(second[1].payload, [0x20, 0xBB]);
        let mut expected = vec![METADATA_TYPE_ITUT_T35];
        expected.extend_from_slice(&big);
        expected.push(0x80);
        assert_eq!(second[2].payload, expected);
        assert!(!dir.path().join("00001.ivf.part").exists());
    }

    #[test]
    fn extension_headers_and_still_pictures_are_read_right() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("00001.ivf");
        let with_extension = [vec![(OBU_FRAME << 3) | 0x06, 0x08, 0x02], vec![0x10, 0xAA]].concat();
        // reduced_still_picture_header: no show bits, every frame is shown.
        let tus = [
            [obu(OBU_SEQUENCE_HEADER, &[0x08]), obu(OBU_FRAME, &[0x00, 0xBB])].concat(),
            with_extension.clone(),
        ];
        std::fs::write(&path, ivf(&tus)).unwrap();

        insert_t35_metadata(&path, &[vec![vec![0xB5, 1]], vec![vec![0xB5, 2]]]).unwrap();

        let out = frames(&std::fs::read(&path).unwrap());
        assert_eq!(kinds(&out[0]), [OBU_SEQUENCE_HEADER, OBU_METADATA, OBU_FRAME]);
        assert_eq!(kinds(&out[1]), [OBU_METADATA, OBU_FRAME]);
        assert!(out[1].ends_with(&with_extension));
    }

    #[test]
    fn a_frame_count_that_does_not_match_the_decode_is_an_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("00001.ivf");
        let tu = [obu(TD, &[]), obu(OBU_FRAME, &[0x10])].concat();
        std::fs::write(&path, ivf(&[tu.clone(), tu])).unwrap();

        assert!(insert_t35_metadata(&path, &[vec![]]).is_err());
        assert!(insert_t35_metadata(&path, &[vec![], vec![], vec![]]).is_err());
    }

    #[test]
    fn joined_chunks_count_on_across_the_seam() {
        let dir = tempfile::TempDir::new().unwrap();
        let tu = |b: u8| [obu(TD, &[]), obu(OBU_FRAME, &[0x10, b])].concat();
        let a = dir.path().join("a.ivf");
        let b = dir.path().join("b.ivf");
        std::fs::write(&a, ivf(&[tu(1), tu(2)])).unwrap();
        std::fs::write(&b, ivf(&[tu(3)])).unwrap();

        let out = dir.path().join("video.ivf");
        assert_eq!(concat_ivf(&[a, b], &out).unwrap(), 3);

        let data = std::fs::read(&out).unwrap();
        assert_eq!(u32::from_le_bytes(data[24..28].try_into().unwrap()), 3);
        let pts: Vec<u64> = (0..3)
            .scan(32usize, |pos, _| {
                let size = u32::from_le_bytes(data[*pos..*pos + 4].try_into().unwrap()) as usize;
                let pts = u64::from_le_bytes(data[*pos + 4..*pos + 12].try_into().unwrap());
                *pos += 12 + size;
                Some(pts)
            })
            .collect();
        assert_eq!(pts, [0, 1, 2]);
        assert_eq!(frames(&data), [tu(1), tu(2), tu(3)]);

        let mut other_rate = ivf(&[tu(4)]);
        other_rate[16..20].copy_from_slice(&30u32.to_le_bytes());
        let c = dir.path().join("c.ivf");
        std::fs::write(&c, other_rate).unwrap();
        assert!(concat_ivf(&[dir.path().join("a.ivf"), c], &out).is_err());
    }

    use proptest::prelude::*;

    #[derive(Debug, Clone)]
    enum Picture { Hidden(Vec<u8>), Shown(Vec<u8>), ShowExisting(Vec<u8>) }

    fn picture() -> impl Strategy<Value = Picture> {
        let body = prop::collection::vec(any::<u8>(), 0..40);
        prop_oneof![
            (any::<u8>(), body.clone()).prop_map(|(b, rest)| Picture::Hidden([vec![b & 0x6F], rest].concat())),
            (any::<u8>(), body.clone()).prop_map(|(b, rest)| Picture::Shown([vec![(b & 0x7F) | 0x10], rest].concat())),
            (any::<u8>(), body).prop_map(|(b, rest)| Picture::ShowExisting([vec![b | 0x80], rest].concat())),
        ]
    }

    /// Temporal units of hidden frames followed by one shown frame, with padding around.
    fn temporal_units() -> impl Strategy<Value = Vec<Vec<u8>>> {
        let tu = (prop::collection::vec(picture(), 0..4), picture(), any::<bool>(), prop::collection::vec(any::<u8>(), 0..300))
            .prop_map(|(mut hidden, shown, pad, padding)| {
                hidden.retain(|p| matches!(p, Picture::Hidden(_)));
                let shown = match shown { Picture::Hidden(b) => Picture::Shown([vec![0x10], b].concat()), p => p };
                let mut out = obu(TD, &[]);
                for p in hidden.iter().chain(std::iter::once(&shown)) {
                    out.extend(match p {
                        Picture::Hidden(b) | Picture::Shown(b) => obu(OBU_FRAME, b),
                        Picture::ShowExisting(b) => obu(OBU_FRAME_HEADER, b),
                    });
                }
                if pad { out.extend(obu(15, &padding)); }
                out
            });
        prop::collection::vec(tu, 1..12).prop_map(|mut tus| {
            tus[0] = [obu(OBU_SEQUENCE_HEADER, &[0x00, 0x00]), tus[0].clone()].concat();
            tus
        })
    }

    proptest! {
        #[test]
        fn inserting_metadata_changes_nothing_but_the_metadata(
            tus in temporal_units(),
            seed in prop::collection::vec(prop::collection::vec(prop::collection::vec(any::<u8>(), 1..200), 0..3), 12),
        ) {
            let dir = tempfile::TempDir::new().unwrap();
            let path = dir.path().join("c.ivf");
            std::fs::write(&path, ivf(&tus)).unwrap();
            let messages: Vec<Vec<Vec<u8>>> = seed[..tus.len()].to_vec();

            insert_t35_metadata(&path, &messages).unwrap();
            let out = frames(&std::fs::read(&path).unwrap());
            prop_assert_eq!(out.len(), tus.len());

            for ((got, original), wanted) in out.iter().zip(&tus).zip(&messages) {
                let obus = parse_obus(got).unwrap();
                let stripped: Vec<u8> = obus.iter().filter(|o| o.kind != OBU_METADATA).flat_map(|o| o.raw.to_vec()).collect();
                prop_assert_eq!(&stripped, original);

                let shown = obus.iter().position(|o| is_shown_frame(o, false)).unwrap();
                let before: Vec<&[u8]> = obus[..shown].iter().rev().take_while(|o| o.kind == OBU_METADATA)
                    .map(|o| o.payload).collect::<Vec<_>>().into_iter().rev().collect();
                prop_assert_eq!(before.len(), wanted.len());
                for (payload, message) in before.iter().zip(wanted) {
                    let expected = [&[METADATA_TYPE_ITUT_T35][..], message, &[0x80]].concat();
                    prop_assert_eq!(*payload, expected.as_slice());
                }
                prop_assert_eq!(obus.iter().filter(|o| o.kind == OBU_METADATA).count(), wanted.len());
            }
        }

        #[test]
        fn joining_keeps_every_frame_in_order(sizes in prop::collection::vec(1usize..20, 1..8)) {
            let dir = tempfile::TempDir::new().unwrap();
            let mut inputs = Vec::new();
            let mut expected = Vec::new();
            for (c, n) in sizes.iter().enumerate() {
                let tus: Vec<Vec<u8>> = (0..*n).map(|i| [obu(TD, &[]), obu(OBU_FRAME, &[0x10, c as u8, i as u8])].concat()).collect();
                let path = dir.path().join(format!("{c}.ivf"));
                std::fs::write(&path, ivf(&tus)).unwrap();
                expected.extend(tus);
                inputs.push(path);
            }
            let out = dir.path().join("video.ivf");
            prop_assert_eq!(concat_ivf(&inputs, &out).unwrap(), expected.len() as u64);
            let data = std::fs::read(&out).unwrap();
            prop_assert_eq!(frames(&data), expected);
        }

        #[test]
        fn leb128_round_trips(value in 0usize..(1 << 56)) {
            let mut buf = Vec::new();
            write_leb128(value, &mut buf);
            prop_assert_eq!(read_leb128(&buf, 0), Some((value as u64, buf.len())));
        }
    }

    #[test]
    fn av1c_keeps_the_sequence_header_and_static_hdr_only() {
        let seq = obu(OBU_SEQUENCE_HEADER, &[0x00, 0x00, 0x00]);
        let mdcv = obu(OBU_METADATA, &[METADATA_TYPE_HDR_MDCV, 1, 2, 0x80]);
        let t35 = obu(OBU_METADATA, &[METADATA_TYPE_ITUT_T35, 0xB5, 0x00, 0x3C, 0x80]);
        let av1c = [vec![0x81, 0x05, 0x4c, 0x00], seq.clone(), mdcv.clone(), t35].concat();

        let trimmed = av1c_without_frame_metadata(&av1c).unwrap();
        assert_eq!(trimmed, [vec![0x81, 0x05, 0x4c, 0x00], seq, mdcv].concat());
        assert_eq!(av1c_without_frame_metadata(&trimmed), None);
    }
}
