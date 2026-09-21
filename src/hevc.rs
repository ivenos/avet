use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufReader, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

use crate::ext::external_bin;

const NAL_SEI_PREFIX: u8 = 39;
const NAL_SEI_SUFFIX: u8 = 40;
const SEI_USER_DATA_REGISTERED_ITU_T_T35: u32 = 4;
use crate::hdr::HDR10PLUS_T35_HEADER;

/// ST 2094-40 from application_version on, for every frame in display order.
pub type Hdr10PlusFrames = Arc<Vec<Option<Arc<[u8]>>>>;

fn remove_emulation_prevention(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut zeros = 0;
    for &b in data {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

/// The HDR10+ message of an SEI NAL payload (after the NAL header), if it has one.
fn hdr10plus_in_sei(nal_payload: &[u8]) -> Option<Vec<u8>> {
    let rbsp = remove_emulation_prevention(nal_payload);
    let mut pos = 0;
    let read_value = |pos: &mut usize| -> Option<u32> {
        let mut value = 0u32;
        loop {
            let b = *rbsp.get(*pos)?;
            *pos += 1;
            // H.265 does not cap the ff_byte run, so the sum can leave u32.
            value = value.checked_add(u32::from(b))?;
            if b != 0xFF {
                return Some(value);
            }
        }
    };
    while pos < rbsp.len() && rbsp[pos..] != [0x80] {
        let payload_type = read_value(&mut pos)?;
        let size = read_value(&mut pos)? as usize;
        let payload = rbsp.get(pos..pos + size)?;
        if payload_type == SEI_USER_DATA_REGISTERED_ITU_T_T35 && payload.starts_with(&HDR10PLUS_T35_HEADER) {
            return Some(payload[HDR10PLUS_T35_HEADER.len()..].to_vec());
        }
        pos += size;
    }
    None
}

#[derive(Default)]
struct AccessUnits {
    hdr10plus: Vec<Option<Vec<u8>>>,
    pending: Option<Vec<u8>>,
}

impl AccessUnits {
    fn nal(&mut self, nal: &[u8]) {
        let &[h0, h1, ..] = nal else { return };
        if (h0 & 0x01) != 0 || (h1 >> 3) != 0 {
            return;
        }
        match (h0 >> 1) & 0x3F {
            // A VCL NAL with first_slice_segment_in_pic_flag starts the next picture.
            0..=31 if nal.get(2).is_some_and(|b| b & 0x80 != 0) => {
                self.hdr10plus.push(self.pending.take());
            }
            NAL_SEI_PREFIX => {
                if let Some(m) = hdr10plus_in_sei(&nal[2..]) {
                    self.pending = Some(m);
                }
            }
            NAL_SEI_SUFFIX => {
                if let (Some(m), Some(last)) = (hdr10plus_in_sei(&nal[2..]), self.hdr10plus.last_mut()) {
                    *last = Some(m);
                }
            }
            _ => {}
        }
    }
}

/// Index just past the next `00 00 01` at or after `from`.
fn next_start_code(buf: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 3 <= buf.len() {
        let one = buf[i + 2..].iter().position(|&b| b == 1)? + i + 2;
        if one >= 2 && buf[one - 1] == 0 && buf[one - 2] == 0 && one - 2 >= from {
            return Some(one + 1);
        }
        i = one - 1;
    }
    None
}

/// HDR10+ of each access unit in decode order, from an Annex B stream.
fn hdr10plus_by_access_unit(mut r: impl Read) -> Result<Vec<Option<Vec<u8>>>> {
    let mut units = AccessUnits::default();
    let mut buf = Vec::new();
    let mut read = vec![0u8; 1 << 20];
    let mut nal_start: Option<usize> = None;
    let mut scan_from = 0;
    loop {
        let n = r.read(&mut read).context("read the HEVC stream")?;
        buf.extend_from_slice(&read[..n]);
        while let Some(next) = next_start_code(&buf, scan_from) {
            if let Some(start) = nal_start {
                let mut end = next - 3;
                while end > start && buf[end - 1] == 0 {
                    end -= 1;
                }
                units.nal(&buf[start..end]);
            }
            nal_start = Some(next);
            scan_from = next;
        }
        if n == 0 {
            if let Some(start) = nal_start {
                units.nal(&buf[start..]);
            }
            return Ok(units.hdr10plus);
        }
        // Keep the unfinished NAL, and two bytes a start code may continue from.
        let keep_from = nal_start.unwrap_or(buf.len()).min(buf.len().saturating_sub(2));
        buf.drain(..keep_from);
        nal_start = nal_start.map(|s| s - keep_from);
        scan_from = buf.len().saturating_sub(2).max(nal_start.unwrap_or(0));
    }
}

/// Carries each message on to the frames after it in display order, as a decoder that
/// plays from the start would. FFmpeg's does so only until the next seek.
fn in_display_order(pts: &[Option<i64>], units: Vec<Option<Vec<u8>>>) -> Result<Vec<Option<Arc<[u8]>>>> {
    ensure!(
        pts.len() == units.len(),
        "{} video packets but {} pictures in the bitstream", pts.len(), units.len()
    );
    let pts = pts.iter().map(|p| p.context("a video packet has no timestamp")).collect::<Result<Vec<_>>>()?;
    let mut order: Vec<usize> = (0..units.len()).collect();
    order.sort_by_key(|&i| pts[i]);

    let units: Vec<Option<Arc<[u8]>>> = units.into_iter().map(|u| u.map(Arc::from)).collect();
    let mut current = None;
    Ok(order
        .into_iter()
        .map(|i| {
            if let Some(m) = &units[i] {
                current = Some(Arc::clone(m));
            }
            current.clone()
        })
        .collect())
}

fn packet_pts(source: &Path) -> Result<Vec<Option<i64>>> {
    #[derive(Deserialize)]
    struct Probe { #[serde(default)] packets: Vec<Packet> }
    #[derive(Deserialize)]
    struct Packet { pts: Option<i64> }

    // Demuxes the whole file.
    const TIMEOUT_SECS: u64 = 3600;
    let probe: Probe = crate::ext::ffprobe_json_with_timeout(
        &["-v", "error", "-select_streams", "v:0", "-show_entries", "packet=pts", "-of", "json"],
        source,
        TIMEOUT_SECS,
    )?;
    Ok(probe.packets.into_iter().map(|p| p.pts).collect())
}

fn scan(source: &Path) -> Result<Vec<Option<Arc<[u8]>>>> {
    let mut child = Command::new(external_bin("ffmpeg"))
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(source)
        // -copyinkf: a stream cut mid-GOP starts with pictures ffmpeg would drop, but ffprobe counts.
        .args(["-map", "0:v:0", "-c:v", "copy", "-copyinkf", "-bsf:v", "hevc_mp4toannexb", "-f", "hevc", "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("start ffmpeg to read the HEVC stream")?;
    let stdout = child.stdout.take().expect("ffmpeg stdout unavailable");
    let mut stderr = child.stderr.take().expect("ffmpeg stderr unavailable");

    let (pts, units, err) = std::thread::scope(|s| {
        let pts = s.spawn(|| packet_pts(source));
        let err = s.spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            text
        });
        let units = hdr10plus_by_access_unit(BufReader::new(stdout));
        (pts.join(), units, err.join().unwrap_or_default())
    });

    let status = child.wait().context("wait for ffmpeg")?;
    // The reader first: one that gave up closed the pipe, and ffmpeg's SIGPIPE is only the echo.
    let units = units.context("read the HDR10+ messages")?;
    let pts = pts.map_err(|_| anyhow::anyhow!("packet probe panicked"))??;
    if !status.success() {
        return Err(crate::ext::tool_error("ffmpeg reading the HEVC stream", status, &err));
    }
    in_display_order(&pts, units)
}

#[derive(Serialize, Deserialize)]
struct Cache {
    messages: Vec<String>,
    frames: Vec<Option<usize>>,
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

fn save(frames: &[Option<Arc<[u8]>>], path: &Path) -> Result<()> {
    let mut cache = Cache { messages: Vec::new(), frames: Vec::with_capacity(frames.len()) };
    let mut last: Option<&Arc<[u8]>> = None;
    for frame in frames {
        cache.frames.push(frame.as_ref().map(|m| {
            if !last.is_some_and(|l| Arc::ptr_eq(l, m)) {
                cache.messages.push(to_hex(m));
                last = Some(m);
            }
            cache.messages.len() - 1
        }));
    }
    crate::resume::write_json_atomic(path, &cache)
}

fn load(path: &Path) -> Result<Vec<Option<Arc<[u8]>>>> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let cache: Cache = serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
    let messages = cache.messages.iter()
        .map(|m| from_hex(m).map(Arc::<[u8]>::from).context("invalid hex"))
        .collect::<Result<Vec<_>>>()?;
    cache.frames.into_iter()
        .map(|f| f.map(|i| messages.get(i).cloned().context("message index out of range")).transpose())
        .collect()
}

/// HDR10+ per frame from the bitstream, cached at `cache` since it reads the whole file.
pub fn hdr10plus_frames(source: &Path, cache: &Path) -> Result<Hdr10PlusFrames> {
    if cache.exists() {
        return load(cache).map(Arc::new);
    }
    let frames = scan(source)?;
    save(&frames, cache)?;
    Ok(Arc::new(frames))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nal(nal_type: u8, payload: &[u8]) -> Vec<u8> {
        [vec![0, 0, 0, 1, nal_type << 1, 0x01], payload.to_vec()].concat()
    }

    fn sei(messages: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut rbsp = Vec::new();
        for (kind, payload) in messages {
            rbsp.push(*kind);
            let mut size = payload.len();
            while size >= 255 {
                rbsp.push(0xFF);
                size -= 255;
            }
            rbsp.push(size as u8);
            rbsp.extend_from_slice(payload);
        }
        rbsp.push(0x80);
        rbsp
    }

    fn hdr10plus(value: u8) -> (u8, Vec<u8>) {
        (4, [&HDR10PLUS_T35_HEADER[..], &[0x01, 0x40, value]].concat())
    }

    #[test]
    fn an_endless_run_of_ff_bytes_is_no_message() {
        let ff_bytes = (u32::MAX / 255) as usize + 16;
        assert_eq!(hdr10plus_in_sei(&vec![0xFF; ff_bytes]), None);
    }

    /// Reads a few bytes at a time, so NALs and start codes straddle every read.
    struct Trickle<'a>(&'a [u8]);

    impl Read for Trickle<'_> {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let n = self.0.len().min(out.len()).min(5);
            out[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }

    use proptest::prelude::*;

    #[derive(Debug, Clone)]
    enum Nal { FirstSlice(Vec<u8>), NextSlice(Vec<u8>), Prefix(Option<Vec<u8>>), Suffix(Option<Vec<u8>>), Other, EnhancementSlice }

    fn escape(rbsp: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut zeros = 0;
        for &b in rbsp {
            if zeros >= 2 && b <= 3 {
                out.push(3);
                zeros = 0;
            }
            out.push(b);
            zeros = if b == 0 { zeros + 1 } else { 0 };
        }
        out
    }

    fn nal_strategy() -> impl Strategy<Value = Nal> {
        let bytes = prop::collection::vec(prop_oneof![Just(0u8), Just(3u8), any::<u8>()], 1..400);
        prop_oneof![
            bytes.clone().prop_map(Nal::FirstSlice),
            bytes.clone().prop_map(Nal::NextSlice),
            prop::option::of(bytes.clone()).prop_map(Nal::Prefix),
            prop::option::of(bytes).prop_map(Nal::Suffix),
            Just(Nal::Other),
            Just(Nal::EnhancementSlice),
        ]
    }

    struct Chunked<'a> { data: &'a [u8], sizes: Vec<usize>, i: usize }

    impl Read for Chunked<'_> {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let n = self.data.len().min(out.len()).min(self.sizes[self.i % self.sizes.len()]);
            self.i += 1;
            out[..n].copy_from_slice(&self.data[..n]);
            self.data = &self.data[n..];
            Ok(n)
        }
    }

    proptest! {
        #[test]
        fn every_picture_gets_its_message_whatever_the_byte_layout(
            nals in prop::collection::vec((nal_strategy(), any::<bool>(), 0usize..3), 1..60),
            sizes in prop::collection::vec(prop_oneof![1usize..5, 1usize..5000], 1..8),
        ) {
            let mut stream = Vec::new();
            let mut expected: Vec<Option<Vec<u8>>> = Vec::new();
            let mut pending = None;
            for (nal, long_start, trailing_zeros) in &nals {
                stream.extend_from_slice(if *long_start { &[0, 0, 0, 1][..] } else { &[0, 0, 1][..] });
                let (header, rbsp): ([u8; 2], Vec<u8>) = match nal {
                    Nal::FirstSlice(b) | Nal::NextSlice(b) => {
                        let first = matches!(nal, Nal::FirstSlice(_));
                        if first { expected.push(pending.take()); }
                        ([0x02, 0x01], [vec![if first { 0x80 } else { 0x00 }], b.clone(), vec![0x80]].concat())
                    }
                    Nal::Prefix(m) | Nal::Suffix(m) => {
                        let prefix = matches!(nal, Nal::Prefix(_));
                        let messages = match m {
                            Some(v) => {
                                if prefix { pending = Some(v.clone()); } else if let Some(last) = expected.last_mut() { *last = Some(v.clone()); }
                                vec![(5, vec![0x11; 20]), (4, [&HDR10PLUS_T35_HEADER[..], v].concat())]
                            }
                            None => vec![(4, vec![0xB5, 0x00, 0x31, 0x47])],
                        };
                        ([if prefix { NAL_SEI_PREFIX } else { NAL_SEI_SUFFIX } << 1, 0x01], sei(&messages))
                    }
                    Nal::Other => ([33 << 1, 0x01], vec![0x42, 0x80]),
                    Nal::EnhancementSlice => ([0x02, 0x09], vec![0x80, 0x80]),
                };
                stream.extend_from_slice(&header);
                stream.extend(escape(&rbsp));
                stream.extend(std::iter::repeat_n(0u8, *trailing_zeros));
            }

            let got = hdr10plus_by_access_unit(Chunked { data: &stream, sizes, i: 0 }).unwrap();
            prop_assert_eq!(got, expected);
        }

        #[test]
        fn display_order_carries_the_last_message_on(
            order in Just((0..40).collect::<Vec<i64>>()).prop_shuffle(),
            messages in prop::collection::vec(prop::option::of(any::<u8>()), 40),
        ) {
            let pts: Vec<Option<i64>> = order.iter().map(|&p| Some(p)).collect();
            let units: Vec<Option<Vec<u8>>> = messages.iter().map(|m| m.map(|b| vec![b])).collect();
            let frames = in_display_order(&pts, units).unwrap();

            let mut by_pts: Vec<(i64, Option<u8>)> = order.iter().copied().zip(messages.iter().copied()).collect();
            by_pts.sort();
            let mut current = None;
            for ((_, m), frame) in by_pts.iter().zip(&frames) {
                if m.is_some() { current = *m; }
                prop_assert_eq!(frame.as_ref().map(|f| f[0]), current);
            }
        }
    }

    #[test]
    fn sei_messages_are_read_past_other_payloads_and_emulation_prevention() {
        let other_t35 = (4, vec![0xB5, 0x00, 0x31, 0x47, 0x41]);
        let long = (5, vec![0x11; 300]);
        let needs_escape = (4, [&HDR10PLUS_T35_HEADER[..], &[0x01, 0x00, 0x00, 0x03, 9]].concat());
        let raw = sei(&[other_t35, long, needs_escape]);
        let with_epb: Vec<u8> = raw.iter().flat_map(|&b| if b == 3 { vec![3, 3] } else { vec![b] }).collect();

        assert_eq!(hdr10plus_in_sei(&with_epb), Some(vec![0x01, 0x00, 0x00, 0x03, 9]));
        assert_eq!(hdr10plus_in_sei(&sei(&[(5, vec![1, 2])])), None);
    }

    #[test]
    fn messages_go_to_the_picture_they_precede_or_follow() {
        let slice = |first: bool| nal(1, &[if first { 0x80 } else { 0x00 }, 0xAA, 0x00, 0x00, 0x00]);
        let stream = [
            nal(32, &[0x0C]), nal(33, &[0x01]), nal(34, &[0x02]),
            nal(NAL_SEI_PREFIX, &sei(&[hdr10plus(1)])), slice(true), slice(false),
            slice(true),
            slice(true), nal(NAL_SEI_SUFFIX, &sei(&[hdr10plus(3)])),
            // An enhancement layer picture is not a picture of its own.
            [vec![0, 0, 1, 0x02, 0x09], vec![0x80]].concat(),
        ]
        .concat();

        let units = hdr10plus_by_access_unit(Trickle(&stream)).unwrap();
        let values: Vec<Option<u8>> = units.iter().map(|u| u.as_ref().map(|m| m[2])).collect();
        assert_eq!(values, [Some(1), None, Some(3)]);
    }

    #[test]
    fn values_carry_on_in_display_order_not_decode_order() {
        // Decode order I0 P4 B2 B1 B3 I5, with a new message on P4 only.
        let pts = [0, 4, 2, 1, 3, 5].map(Some);
        let units = vec![Some(vec![10]), Some(vec![40]), None, None, None, None];
        let frames = in_display_order(&pts, units).unwrap();
        let values: Vec<Option<u8>> = frames.iter().map(|f| f.as_ref().map(|m| m[0])).collect();
        assert_eq!(values, [Some(10), Some(10), Some(10), Some(10), Some(40), Some(40)]);

        assert!(in_display_order(&[Some(0)], vec![None, None]).is_err());
        assert!(in_display_order(&[None], vec![None]).is_err());
    }

    #[test]
    fn the_cache_round_trips_and_shares_repeated_messages() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("hdr10plus.json");
        let a: Arc<[u8]> = Arc::from(vec![1u8, 2, 3]);
        let b: Arc<[u8]> = Arc::from(vec![0xFFu8]);
        let frames = vec![None, Some(a.clone()), Some(a), Some(b.clone()), Some(b)];

        save(&frames, &path).unwrap();
        let raw: Cache = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(raw.messages, ["010203", "ff"]);
        assert_eq!(load(&path).unwrap(), frames);
    }
}
